use std::{env, sync::OnceLock, time::Duration};

use anyhow::{Context, Result, anyhow};
use reqx::blocking::{Client, ClientBuilder};
use semver::Version;
use serde::Deserialize;

const GITHUB_API_HOST: &str = "https://api.github.com";
const LATEST_RELEASE_PATH: &str = "/repos/oksyd/spotlit/releases/latest";
const GITHUB_API_VERSION: &str = "2026-03-10";
const RELEASE_METADATA_MAX_BYTES: usize = 64 * 1024;
const PROXY_ENV_KEYS: &[&str] = &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
const USER_AGENT: &str = concat!(
    "Spotlit/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/oksyd/spotlit)"
);

pub(crate) const RELEASES_URL: &str = "https://github.com/oksyd/spotlit/releases/latest";

static GITHUB_CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UpdateCheck {
    NoRelease,
    UpToDate { latest: Version },
    Available { latest: Version },
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

pub(crate) fn check_for_update() -> Result<UpdateCheck> {
    let response = github_client()?
        .get(LATEST_RELEASE_PATH)
        .try_header("User-Agent", USER_AGENT)
        .context("build GitHub release request")?
        .try_header("Accept", "application/vnd.github+json")
        .context("build GitHub release request")?
        .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .context("build GitHub release request")?
        .max_response_body_bytes(RELEASE_METADATA_MAX_BYTES)
        .send_response()
        .context("fetch latest GitHub release")?;

    if !release_response_has_body(response.status().as_u16())? {
        return Ok(UpdateCheck::NoRelease);
    }

    let release = response
        .json::<GitHubRelease>()
        .context("parse latest GitHub release")?;

    evaluate_release(env!("CARGO_PKG_VERSION"), &release.tag_name)
}

fn evaluate_release(current_version: &str, release_tag: &str) -> Result<UpdateCheck> {
    let current = Version::parse(current_version)
        .with_context(|| format!("parse current version {current_version}"))?;
    let latest = parse_release_tag(release_tag)?;

    if latest > current {
        Ok(UpdateCheck::Available { latest })
    } else {
        Ok(UpdateCheck::UpToDate { latest })
    }
}

fn release_response_has_body(status: u16) -> Result<bool> {
    match status {
        404 => Ok(false),
        200..=299 => Ok(true),
        _ => Err(anyhow!("GitHub release request returned HTTP {status}")),
    }
}

fn parse_release_tag(tag: &str) -> Result<Version> {
    let tag = tag.trim();
    let version = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    Version::parse(version).map_err(|error| anyhow!("invalid Spotlit release tag {tag:?}: {error}"))
}

fn github_client() -> Result<&'static Client> {
    GITHUB_CLIENT
        .get_or_init(|| {
            let builder = Client::builder(GITHUB_API_HOST)
                .client_name("spotlit-update")
                .request_timeout(Duration::from_secs(10))
                .total_timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(8))
                .max_response_body_bytes(RELEASE_METADATA_MAX_BYTES);

            configure_proxy(builder)?
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow!("initialize GitHub HTTP client: {error}"))
}

fn configure_proxy(builder: ClientBuilder) -> std::result::Result<ClientBuilder, String> {
    let Some(proxy_url) = PROXY_ENV_KEYS
        .iter()
        .filter_map(|name| env::var(name).ok())
        .find(|value| !value.trim().is_empty())
    else {
        return Ok(builder);
    };

    let proxy_uri = proxy_url
        .parse()
        .map_err(|error| format!("parse GitHub proxy URI from environment: {error}"))?;
    Ok(builder.http_proxy(proxy_uri))
}

#[cfg(test)]
mod tests {
    use super::{UpdateCheck, evaluate_release, parse_release_tag, release_response_has_body};

    #[test]
    fn newer_release_is_available() {
        assert_eq!(
            evaluate_release("0.1.0", "v0.2.0").unwrap(),
            UpdateCheck::Available {
                latest: "0.2.0".parse().unwrap()
            }
        );
    }

    #[test]
    fn equal_or_older_release_is_up_to_date() {
        assert!(matches!(
            evaluate_release("0.2.0", "0.2.0").unwrap(),
            UpdateCheck::UpToDate { .. }
        ));
        assert!(matches!(
            evaluate_release("0.2.0", "v0.1.9").unwrap(),
            UpdateCheck::UpToDate { .. }
        ));
    }

    #[test]
    fn prerelease_tags_follow_semver_ordering() {
        assert!(matches!(
            evaluate_release("0.1.0", "V0.2.0-beta.1").unwrap(),
            UpdateCheck::Available { .. }
        ));
    }

    #[test]
    fn malformed_release_tag_is_rejected() {
        assert!(parse_release_tag("release-next").is_err());
    }

    #[test]
    fn release_http_statuses_are_classified() {
        assert!(!release_response_has_body(404).unwrap());
        assert!(release_response_has_body(200).unwrap());
        assert!(release_response_has_body(503).is_err());
    }
}
