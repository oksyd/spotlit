use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, ensure};
use reqx::{
    blocking::{Client, ClientBuilder},
    prelude::RedirectPolicy,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::{UpdateProxy, UpdateProxyMode};

const GITHUB_API_HOST: &str = "https://api.github.com";
const LATEST_RELEASE_PATH: &str = "/repos/oksyd/spotlit/releases/latest";
const RELEASE_ASSET_PATH_PREFIX: &str = "/repos/oksyd/spotlit/releases/assets/";
const GITHUB_API_VERSION: &str = "2026-03-10";
const RELEASE_METADATA_MAX_BYTES: usize = 64 * 1024;
const CHECKSUMS_MAX_BYTES: usize = 64 * 1024;
const PACKAGE_MAX_BYTES: u64 = 128 * 1024 * 1024;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const UPDATE_RETRY_INTERVAL: Duration = Duration::from_secs(15 * 60);
const UPDATE_STATE_FILE: &str = "state.json";
const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS";
const PROXY_ENV_KEYS: &[&str] = &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
const USER_AGENT: &str = concat!(
    "Spotlit/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/oksyd/spotlit)"
);
const DISTRIBUTION: Option<&str> = option_env!("SPOTLIT_DISTRIBUTION");
#[cfg(windows)]
const APPLY_UPDATE_FLAG: &str = "--apply-update";

pub(crate) const RELEASES_URL: &str = "https://github.com/oksyd/spotlit/releases/latest";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum UpdateCheck {
    NoRelease,
    UpToDate { release: ReleaseInfo },
    Available { release: ReleaseInfo },
}

impl UpdateCheck {
    pub(crate) fn release(&self) -> Option<&ReleaseInfo> {
        match self {
            Self::NoRelease => None,
            Self::UpToDate { release } | Self::Available { release } => Some(release),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ReleaseInfo {
    tag_name: String,
    pub(crate) version: Version,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct PreparedUpdate {
    pub(crate) version: Version,
    package_path: PathBuf,
    size: u64,
    sha256: String,
    #[cfg(windows)]
    archive_root: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InstallDisposition {
    #[cfg(target_os = "linux")]
    Installed,
    #[cfg(target_os = "linux")]
    Cancelled,
    #[cfg(windows)]
    Restarting,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Eq, PartialEq)]
struct ReleaseAsset {
    id: u64,
    name: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct UpdateCache {
    last_checked_unix: Option<u64>,
    #[serde(default)]
    last_attempt_unix: Option<u64>,
    #[serde(default)]
    last_check_failed: bool,
    release: Option<GitHubRelease>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PackageTarget {
    #[cfg(target_os = "linux")]
    LinuxDeb,
    #[cfg(any(windows, test))]
    WindowsZip,
}

struct DownloadSpec<'a> {
    package: &'a ReleaseAsset,
    checksums: &'a ReleaseAsset,
}

pub(crate) fn check_for_update(proxy: &UpdateProxy) -> Result<UpdateCheck> {
    let client = build_github_client(false, proxy)?;
    let response = client
        .get(LATEST_RELEASE_PATH)
        .try_header("User-Agent", USER_AGENT)
        .context("build GitHub release request")?
        .try_header("Accept", "application/vnd.github+json")
        .context("build GitHub release request")?
        .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .context("build GitHub release request")?
        .max_response_body_bytes(RELEASE_METADATA_MAX_BYTES)
        .send()
        .context("fetch latest GitHub release")?;

    if !release_response_has_body(response.status().as_u16())? {
        return Ok(UpdateCheck::NoRelease);
    }

    let release = response
        .json::<GitHubRelease>()
        .context("parse latest GitHub release")?;

    evaluate_release(env!("CARGO_PKG_VERSION"), &release)
}

pub(crate) fn install_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        env::current_exe()
            .is_ok_and(|executable| supports_debian_installation(DISTRIBUTION, &executable))
    }

    #[cfg(windows)]
    {
        DISTRIBUTION == Some("github-windows-portable")
    }
}

pub(crate) fn release_is_downloadable(release: &ReleaseInfo) -> bool {
    install_supported() && download_spec(release, current_package_target()).is_ok()
}

pub(crate) fn automatic_check_due(update_dir: &Path) -> Result<bool> {
    automatic_check_due_at(update_dir, unix_now())
}

pub(crate) fn cached_update_check(update_dir: &Path) -> Result<Option<UpdateCheck>> {
    let Some(cache) = load_cache(update_dir)? else {
        return Ok(None);
    };
    let Some(release) = cache.release else {
        return Ok(cache.last_checked_unix.map(|_| UpdateCheck::NoRelease));
    };
    evaluate_release(env!("CARGO_PKG_VERSION"), &release).map(Some)
}

pub(crate) fn record_check_result(update_dir: &Path, result: &Result<UpdateCheck>) -> Result<()> {
    record_check_result_at(update_dir, result, unix_now())
}

fn record_check_result_at(update_dir: &Path, result: &Result<UpdateCheck>, now: u64) -> Result<()> {
    let mut cache = load_cache(update_dir).ok().flatten().unwrap_or_default();
    cache.last_attempt_unix = Some(now);
    cache.last_check_failed = result.is_err();
    if let Ok(check) = result {
        cache.last_checked_unix = Some(now);
        cache.release = check.release().map(GitHubRelease::from);
    }
    save_cache(update_dir, &cache)
}

pub(crate) fn download_update(
    release: &ReleaseInfo,
    update_dir: &Path,
    proxy: &UpdateProxy,
) -> Result<PreparedUpdate> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    let spec = download_spec(release, current_package_target())?;
    let client = build_github_client(true, proxy)?;
    fs::create_dir_all(update_dir)
        .with_context(|| format!("create Spotlit update directory {}", update_dir.display()))?;

    let checksum_bytes = download_asset_bytes(&client, spec.checksums, CHECKSUMS_MAX_BYTES)
        .context("download release checksums")?;
    verify_downloaded_asset_digest(spec.checksums, &checksum_bytes)?;
    let checksum_text =
        std::str::from_utf8(&checksum_bytes).context("release checksums are not valid UTF-8")?;
    let expected_sha256 = checksum_for_asset(checksum_text, &spec.package.name)?;
    verify_api_digest(spec.package, &expected_sha256)?;

    let package_path = update_dir.join(&spec.package.name);
    if !existing_package_is_valid(&package_path, spec.package.size, &expected_sha256)? {
        download_package(&client, spec.package, &package_path, &expected_sha256)?;
    }

    Ok(PreparedUpdate {
        version: release.version.clone(),
        package_path,
        size: spec.package.size,
        sha256: expected_sha256,
        #[cfg(windows)]
        archive_root: expected_windows_archive_root(release),
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn install_prepared_update(update: &PreparedUpdate) -> Result<InstallDisposition> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    install_debian_update_with(update, |command| command.output())
}

#[cfg(target_os = "linux")]
fn supports_debian_installation(distribution: Option<&str>, executable: &Path) -> bool {
    distribution == Some("github-linux-deb")
        && (executable == Path::new("/usr/bin/spotlit")
            || executable == Path::new("/usr/bin/spotlit (deleted)"))
}

#[cfg(target_os = "linux")]
fn install_debian_update_with(
    update: &PreparedUpdate,
    mut run: impl FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<InstallDisposition> {
    validate_prepared_update(update)?;
    ensure!(
        update
            .package_path
            .extension()
            .is_some_and(|value| value == "deb"),
        "the prepared update is not a Debian package"
    );
    let package_path =
        fs::canonicalize(&update.package_path).context("resolve Debian update path")?;
    let version = update.version.to_string().replacen('-', "~", 1);
    let expected = format!("spotlit\n{version}\namd64");
    let package = run(Command::new("/usr/bin/dpkg-deb")
        .arg("--show")
        .arg("--showformat=${Package}\\n${Version}\\n${Architecture}")
        .arg(&package_path)
        .stdin(Stdio::null()))
    .context("inspect Debian update package")?;
    ensure!(
        package.status.success(),
        "could not inspect Debian update package: {}",
        String::from_utf8_lossy(&package.stderr).trim()
    );
    ensure!(
        String::from_utf8_lossy(&package.stdout).trim() == expected,
        "Debian update package identity does not match the release"
    );

    let mut query = Command::new("/usr/bin/dpkg-query");
    query
        .args([
            "--show",
            "--showformat=${Package}\\n${Version}\\n${Architecture}\\n${db:Status-Status}",
            "spotlit",
        ])
        .stdin(Stdio::null());
    let current = run(&mut query).context("inspect installed Spotlit package")?;
    let current_identity =
        String::from_utf8(current.stdout).context("read installed Spotlit package identity")?;
    let fields = current_identity.lines().collect::<Vec<_>>();
    ensure!(
        current.status.success()
            && matches!(fields.as_slice(), ["spotlit", _, "amd64", "installed"]),
        "this Spotlit executable is not managed by the Debian package manager"
    );
    if fields[1] == version {
        return Ok(InstallDisposition::Installed);
    }

    let output = run(Command::new("/usr/bin/pkexec")
        .arg("--disable-internal-agent")
        .arg("/usr/bin/apt-get")
        .args([
            "--assume-yes",
            "--no-install-recommends",
            "--no-remove",
            "install",
        ])
        .arg(&package_path)
        .stdin(Stdio::null()))
    .context("start authorized Debian update installation")?;
    if output.status.code() == Some(126) {
        return Ok(InstallDisposition::Cancelled);
    }
    ensure!(
        output.status.success(),
        "Debian update installation failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let installed = run(&mut query).context("verify installed Spotlit version")?;
    ensure!(
        installed.status.success()
            && String::from_utf8_lossy(&installed.stdout).trim()
                == format!("{expected}\ninstalled"),
        "the installed Spotlit version does not match the update"
    );
    Ok(InstallDisposition::Installed)
}

#[cfg(target_os = "linux")]
pub(crate) fn restart_installed_update() -> Result<()> {
    use std::os::unix::process::CommandExt;

    Err(Command::new("/usr/bin/spotlit").exec()).context("restart updated Spotlit")
}

pub(crate) fn validate_prepared_update(update: &PreparedUpdate) -> Result<()> {
    ensure!(
        existing_package_is_valid(&update.package_path, update.size, &update.sha256)?,
        "the prepared update is missing or changed; download it again"
    );
    Ok(())
}

#[cfg(windows)]
pub(crate) fn install_prepared_update(update: &PreparedUpdate) -> Result<InstallDisposition> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    validate_prepared_update(update)?;
    let current_exe = env::current_exe().context("resolve current Spotlit executable")?;
    let update_dir = update
        .package_path
        .parent()
        .context("prepared update has no parent directory")?;
    let staged_dir = update_dir.join(format!("staged-{}", update.version));
    extract_windows_package(&update.package_path, &update.archive_root, &staged_dir)?;

    let staged_exe = staged_dir.join("spotlit.exe");
    ensure!(
        staged_exe.is_file(),
        "the update archive does not contain spotlit.exe"
    );
    let next_exe = current_exe.with_extension("exe.update");
    remove_file_if_exists(&next_exe)?;
    fs::copy(&staged_exe, &next_exe).with_context(|| {
        format!(
            "stage the new executable from {} to {}",
            staged_exe.display(),
            next_exe.display()
        )
    })?;

    let helper = update_dir.join("spotlit-update-helper.exe");
    remove_file_if_exists(&helper)?;
    fs::copy(&current_exe, &helper)
        .with_context(|| format!("create update helper {}", helper.display()))?;
    remove_file_if_exists(&update_dir.join("update-error.txt"))?;

    Command::new(&helper)
        .arg(APPLY_UPDATE_FLAG)
        .arg("--target")
        .arg(&current_exe)
        .arg("--next")
        .arg(&next_exe)
        .arg("--staged")
        .arg(&staged_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start update helper {}", helper.display()))?;

    Ok(InstallDisposition::Restarting)
}

pub(crate) fn try_apply_pending_update(args: impl IntoIterator<Item = OsString>) -> Result<bool> {
    #[cfg(not(windows))]
    {
        let _ = args;
        Ok(false)
    }

    #[cfg(windows)]
    {
        let args = args.into_iter().collect::<Vec<_>>();
        if args.first().and_then(|value| value.to_str()) != Some(APPLY_UPDATE_FLAG) {
            return Ok(false);
        }

        let options = parse_apply_update_options(&args)?;
        if let Err(error) = apply_windows_update(&options) {
            write_update_failure(&options.staged_dir, &error);
            let _ = Command::new(&options.target_exe)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            return Err(error);
        }
        Ok(true)
    }
}

fn evaluate_release(current_version: &str, release: &GitHubRelease) -> Result<UpdateCheck> {
    let current = Version::parse(current_version)
        .with_context(|| format!("parse current version {current_version}"))?;
    let latest = parse_release_tag(&release.tag_name)?;
    let release = ReleaseInfo {
        tag_name: release.tag_name.trim().to_string(),
        version: latest.clone(),
        assets: release.assets.clone(),
    };

    if latest > current {
        Ok(UpdateCheck::Available { release })
    } else {
        Ok(UpdateCheck::UpToDate { release })
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

fn current_package_target() -> PackageTarget {
    #[cfg(target_os = "linux")]
    {
        PackageTarget::LinuxDeb
    }

    #[cfg(windows)]
    {
        PackageTarget::WindowsZip
    }
}

fn download_spec(release: &ReleaseInfo, target: PackageTarget) -> Result<DownloadSpec<'_>> {
    let checksums = release
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS_ASSET_NAME)
        .ok_or_else(|| {
            anyhow!(
                "release {} has no {CHECKSUMS_ASSET_NAME} asset",
                release.version
            )
        })?;
    ensure!(
        checksums.size > 0 && checksums.size <= CHECKSUMS_MAX_BYTES as u64,
        "release checksums have an invalid size"
    );

    let package_name = expected_package_name(release, target);
    let package = release
        .assets
        .iter()
        .find(|asset| asset.name == package_name)
        .ok_or_else(|| anyhow!("release {} has no {package_name} asset", release.version))?;
    ensure!(
        package.size > 0 && package.size <= PACKAGE_MAX_BYTES,
        "release package has an invalid size"
    );

    Ok(DownloadSpec { package, checksums })
}

fn expected_package_name(release: &ReleaseInfo, target: PackageTarget) -> String {
    match target {
        #[cfg(target_os = "linux")]
        PackageTarget::LinuxDeb => format!("spotlit_{}_amd64.deb", release.version),
        #[cfg(any(windows, test))]
        PackageTarget::WindowsZip => format!(
            "{}-x86_64-pc-windows-msvc.zip",
            expected_windows_archive_root(release)
        ),
    }
}

#[cfg(any(windows, test))]
fn expected_windows_archive_root(release: &ReleaseInfo) -> String {
    format!("spotlit-{}", release.tag_name)
}

fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String> {
    for line in manifest.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(checksum) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if fields.next().is_some() || name.trim_start_matches('*') != asset_name {
            continue;
        }
        let checksum = checksum.to_ascii_lowercase();
        ensure!(
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "release checksum for {asset_name} is not a SHA-256 digest"
        );
        return Ok(checksum);
    }
    Err(anyhow!(
        "{CHECKSUMS_ASSET_NAME} does not contain {asset_name}"
    ))
}

fn verify_api_digest(asset: &ReleaseAsset, expected_sha256: &str) -> Result<()> {
    let Some(digest) = asset.digest.as_deref() else {
        return Ok(());
    };
    let Some(api_sha256) = digest.strip_prefix("sha256:") else {
        return Ok(());
    };
    ensure!(
        api_sha256.eq_ignore_ascii_case(expected_sha256),
        "GitHub asset digest does not match {CHECKSUMS_ASSET_NAME}"
    );
    Ok(())
}

fn verify_downloaded_asset_digest(asset: &ReleaseAsset, bytes: &[u8]) -> Result<()> {
    let Some(digest) = asset.digest.as_deref() else {
        return Ok(());
    };
    let Some(api_sha256) = digest.strip_prefix("sha256:") else {
        return Ok(());
    };
    let actual_sha256 = bytes_to_lower_hex(Sha256::digest(bytes).as_ref());
    ensure!(
        api_sha256.eq_ignore_ascii_case(&actual_sha256),
        "downloaded release asset {} failed GitHub digest verification",
        asset.name
    );
    Ok(())
}

fn download_asset_bytes(
    client: &Client,
    asset: &ReleaseAsset,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    ensure!(asset.size <= max_bytes as u64, "release asset is too large");
    let response = client
        .get(format!("{RELEASE_ASSET_PATH_PREFIX}{}", asset.id))
        .try_header("User-Agent", USER_AGENT)
        .context("build GitHub asset request")?
        .try_header("Accept", "application/octet-stream")
        .context("build GitHub asset request")?
        .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .context("build GitHub asset request")?
        .send_stream()
        .with_context(|| format!("download release asset {}", asset.name))?;
    ensure!(
        response.status().as_u16() == 200,
        "GitHub asset request returned HTTP {}",
        response.status().as_u16()
    );
    let bytes = response
        .into_bytes_limited(max_bytes)
        .with_context(|| format!("read release asset {}", asset.name))?;
    ensure!(
        bytes.len() as u64 == asset.size,
        "release asset {} size does not match GitHub metadata",
        asset.name
    );
    Ok(bytes.to_vec())
}

fn existing_package_is_valid(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.is_file() || metadata.len() != expected_size {
        return Ok(false);
    }
    Ok(hash_file(path)? == expected_sha256)
}

fn download_package(
    client: &Client,
    asset: &ReleaseAsset,
    destination: &Path,
    expected_sha256: &str,
) -> Result<()> {
    let part_path = destination.with_extension(format!(
        "{}.part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("download")
    ));
    remove_file_if_exists(&part_path)?;

    let result = (|| {
        let mut response = client
            .get(format!("{RELEASE_ASSET_PATH_PREFIX}{}", asset.id))
            .try_header("User-Agent", USER_AGENT)
            .context("build GitHub package request")?
            .try_header("Accept", "application/octet-stream")
            .context("build GitHub package request")?
            .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .context("build GitHub package request")?
            .send_stream()
            .with_context(|| format!("download release package {}", asset.name))?;
        ensure!(
            response.status().as_u16() == 200,
            "GitHub package request returned HTTP {}",
            response.status().as_u16()
        );

        let mut file = File::create(&part_path)
            .with_context(|| format!("create update package {}", part_path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut downloaded = 0_u64;
        loop {
            let read = response
                .read_chunk(&mut buffer)
                .with_context(|| format!("read update package {}", asset.name))?;
            if read == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(read as u64);
            ensure!(
                downloaded <= asset.size && downloaded <= PACKAGE_MAX_BYTES,
                "downloaded update package exceeds its declared size"
            );
            file.write_all(&buffer[..read])
                .with_context(|| format!("write update package {}", part_path.display()))?;
            hasher.update(&buffer[..read]);
        }
        file.sync_all()
            .with_context(|| format!("flush update package {}", part_path.display()))?;
        ensure!(
            downloaded == asset.size,
            "downloaded update package size does not match GitHub metadata"
        );
        let actual_sha256 = bytes_to_lower_hex(hasher.finalize().as_ref());
        ensure!(
            actual_sha256 == expected_sha256,
            "downloaded update package failed SHA-256 verification"
        );

        remove_file_if_exists(destination)?;
        fs::rename(&part_path, destination).with_context(|| {
            format!(
                "commit update package from {} to {}",
                part_path.display(),
                destination.display()
            )
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&part_path);
    }
    result
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {} for hashing", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(bytes_to_lower_hex(hasher.finalize().as_ref()))
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn automatic_check_due_at(update_dir: &Path, now: u64) -> Result<bool> {
    let Some(cache) = load_cache(update_dir)? else {
        return Ok(true);
    };
    let (last_check, interval) = if cache.last_check_failed {
        (cache.last_attempt_unix, UPDATE_RETRY_INTERVAL)
    } else {
        (cache.last_checked_unix, UPDATE_CHECK_INTERVAL)
    };
    let Some(last_checked) = last_check else {
        return Ok(true);
    };
    Ok(now
        .checked_sub(last_checked)
        .is_none_or(|elapsed| elapsed >= interval.as_secs()))
}

fn load_cache(update_dir: &Path) -> Result<Option<UpdateCache>> {
    let path = update_dir.join(UPDATE_STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", path.display()))
        .map(Some)
}

fn save_cache(update_dir: &Path, cache: &UpdateCache) -> Result<()> {
    fs::create_dir_all(update_dir)
        .with_context(|| format!("create update directory {}", update_dir.display()))?;
    let path = update_dir.join(UPDATE_STATE_FILE);
    let temporary = update_dir.join(format!("{UPDATE_STATE_FILE}.new"));
    let bytes = serde_json::to_vec_pretty(cache).context("serialize update state")?;
    fs::write(&temporary, bytes)
        .with_context(|| format!("write update state {}", temporary.display()))?;
    remove_file_if_exists(&path)?;
    fs::rename(&temporary, &path).with_context(|| {
        format!(
            "commit update state from {} to {}",
            temporary.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn build_github_client(download: bool, proxy: &UpdateProxy) -> Result<Client> {
    let builder = Client::builder(GITHUB_API_HOST)
        .client_name(if download {
            "spotlit-update-download"
        } else {
            "spotlit-update-check"
        })
        .request_timeout(Duration::from_secs(if download { 30 } else { 10 }))
        .total_timeout(Some(Duration::from_secs(if download {
            5 * 60
        } else {
            20
        })))
        .connect_timeout(Duration::from_secs(8))
        .redirect_policy(RedirectPolicy::limited(5))
        .max_response_body_bytes(RELEASE_METADATA_MAX_BYTES);

    configure_proxy(builder, proxy)?
        .build()
        .context("initialize GitHub client")
}

pub(crate) fn parse_update_proxy(mode: &str, url: &str) -> Result<UpdateProxy> {
    let mode = match mode {
        "environment" => UpdateProxyMode::Environment,
        "direct" => UpdateProxyMode::Direct,
        "custom" => UpdateProxyMode::Custom,
        _ => anyhow::bail!("Invalid proxy mode"),
    };
    let proxy = UpdateProxy {
        mode,
        url: url.to_string(),
    };
    if mode == UpdateProxyMode::Custom {
        // Building validates the URI without making a network request.
        build_github_client(false, &proxy).map_err(|_| anyhow!("Invalid HTTP proxy address"))?;
    }
    Ok(proxy)
}

fn proxy_url(
    proxy: &UpdateProxy,
    mut env_var: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    match proxy.mode {
        UpdateProxyMode::Environment => PROXY_ENV_KEYS
            .iter()
            .filter_map(|name| env_var(name))
            .map(|value| value.trim().to_string())
            .find(|value| !value.is_empty()),
        UpdateProxyMode::Direct => None,
        UpdateProxyMode::Custom => Some(proxy.url.trim().to_string()),
    }
}

fn configure_proxy(builder: ClientBuilder, proxy: &UpdateProxy) -> Result<ClientBuilder> {
    let Some(proxy_url) = proxy_url(proxy, |name| env::var(name).ok()) else {
        return Ok(builder);
    };

    let proxy_uri = proxy_url
        .parse()
        .map_err(|_| anyhow!("Invalid HTTP proxy address"))?;
    Ok(builder.http_proxy(proxy_uri))
}

impl From<&ReleaseInfo> for GitHubRelease {
    fn from(release: &ReleaseInfo) -> Self {
        Self {
            tag_name: release.tag_name.clone(),
            assets: release.assets.clone(),
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct ApplyUpdateOptions {
    target_exe: PathBuf,
    next_exe: PathBuf,
    staged_dir: PathBuf,
}

#[cfg(windows)]
fn parse_apply_update_options(args: &[OsString]) -> Result<ApplyUpdateOptions> {
    ensure!(args.len() == 7, "invalid update helper arguments");
    ensure!(args[1] == "--target", "missing update target argument");
    ensure!(args[3] == "--next", "missing staged executable argument");
    ensure!(args[5] == "--staged", "missing staged directory argument");
    Ok(ApplyUpdateOptions {
        target_exe: PathBuf::from(args[2].clone()),
        next_exe: PathBuf::from(args[4].clone()),
        staged_dir: PathBuf::from(args[6].clone()),
    })
}

#[cfg(any(windows, test))]
fn extract_windows_package(
    package_path: &Path,
    archive_root: &str,
    staged_dir: &Path,
) -> Result<()> {
    use zip::ZipArchive;

    if staged_dir.exists() {
        fs::remove_dir_all(staged_dir)
            .with_context(|| format!("remove stale update staging {}", staged_dir.display()))?;
    }
    fs::create_dir_all(staged_dir)
        .with_context(|| format!("create update staging {}", staged_dir.display()))?;

    let file = File::open(package_path)
        .with_context(|| format!("open update archive {}", package_path.display()))?;
    let mut archive = ZipArchive::new(file).context("read update ZIP archive")?;
    ensure!(
        archive.len() <= 16,
        "update archive contains too many entries"
    );
    let allowed_files = [
        "spotlit.exe",
        "README.md",
        "LICENSE",
        "THIRD-PARTY-LICENSES.txt",
    ];
    let mut extracted_bytes = 0_u64;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("read update archive entry {index}"))?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| anyhow!("update archive contains an unsafe path"))?;
        let relative = enclosed
            .strip_prefix(archive_root)
            .context("update archive has an unexpected root directory")?;
        if relative.as_os_str().is_empty() || entry.is_dir() {
            continue;
        }
        let relative_name = relative
            .to_str()
            .ok_or_else(|| anyhow!("update archive contains a non-Unicode file name"))?;
        ensure!(
            allowed_files.contains(&relative_name),
            "update archive contains an unexpected file {relative_name}"
        );
        extracted_bytes = extracted_bytes.saturating_add(entry.size());
        ensure!(
            extracted_bytes <= PACKAGE_MAX_BYTES,
            "update archive expands beyond the allowed size"
        );

        let destination = staged_dir.join(relative);
        let mut output = File::create(&destination)
            .with_context(|| format!("create staged update file {}", destination.display()))?;
        std::io::copy(&mut entry, &mut output)
            .with_context(|| format!("extract staged update file {}", destination.display()))?;
        output
            .sync_all()
            .with_context(|| format!("flush staged update file {}", destination.display()))?;
    }
    Ok(())
}

#[cfg(windows)]
fn apply_windows_update(options: &ApplyUpdateOptions) -> Result<()> {
    ensure!(
        options.next_exe.is_file(),
        "staged Spotlit executable is missing"
    );
    ensure!(
        options.target_exe.is_file(),
        "installed Spotlit executable is missing"
    );
    let backup = options.target_exe.with_extension("exe.previous");
    remove_file_if_exists(&backup)?;

    let mut last_error = None;
    for _ in 0..120 {
        match fs::rename(&options.target_exe, &backup) {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
                ) =>
            {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("move current executable to {}", backup.display()));
            }
        }
    }
    if let Some(error) = last_error {
        return Err(error).context("timed out waiting for Spotlit to exit");
    }

    if let Err(error) = fs::rename(&options.next_exe, &options.target_exe) {
        let _ = fs::rename(&backup, &options.target_exe);
        return Err(error).context("activate staged Spotlit executable");
    }

    let launch = Command::new(&options.target_exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = launch {
        let _ = fs::remove_file(&options.target_exe);
        let _ = fs::rename(&backup, &options.target_exe);
        return Err(error).context("restart updated Spotlit");
    }

    let _ = fs::remove_file(&backup);
    copy_windows_release_documents(options);
    let _ = fs::remove_dir_all(&options.staged_dir);
    Ok(())
}

#[cfg(windows)]
fn copy_windows_release_documents(options: &ApplyUpdateOptions) {
    let Some(target_dir) = options.target_exe.parent() else {
        return;
    };
    for name in ["README.md", "LICENSE", "THIRD-PARTY-LICENSES.txt"] {
        let source = options.staged_dir.join(name);
        if source.is_file() {
            let _ = fs::copy(source, target_dir.join(name));
        }
    }
}

#[cfg(windows)]
fn write_update_failure(staged_dir: &Path, error: &anyhow::Error) {
    let Some(update_dir) = staged_dir.parent() else {
        return;
    };
    let _ = fs::write(
        update_dir.join("update-error.txt"),
        format!("Spotlit update failed:\n{error:#}\n"),
    );
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, path::PathBuf};

    use super::{
        GitHubRelease, PackageTarget, ReleaseAsset, UpdateCache, UpdateCheck,
        automatic_check_due_at, bytes_to_lower_hex, checksum_for_asset, download_spec,
        evaluate_release, expected_windows_archive_root, extract_windows_package, load_cache,
        parse_release_tag, record_check_result, record_check_result_at, release_response_has_body,
        save_cache, verify_downloaded_asset_digest,
    };
    use sha2::{Digest, Sha256};

    fn release(tag: &str, assets: Vec<ReleaseAsset>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            assets,
        }
    }

    fn asset(id: u64, name: &str, size: u64) -> ReleaseAsset {
        ReleaseAsset {
            id,
            name: name.to_string(),
            size,
            digest: None,
        }
    }

    #[test]
    fn update_proxy_modes_resolve_without_changing_the_environment() {
        use crate::core::{UpdateProxy, UpdateProxyMode};

        let environment = UpdateProxy::default();
        assert_eq!(
            super::proxy_url(&environment, |name| match name {
                "HTTPS_PROXY" => Some("  ".to_string()),
                "https_proxy" => Some(" http://localhost:7890 ".to_string()),
                "HTTP_PROXY" => Some("http://localhost:8080".to_string()),
                _ => None,
            }),
            Some("http://localhost:7890".to_string())
        );
        assert_eq!(super::proxy_url(&environment, |_| None), None);

        for mode in [UpdateProxyMode::Direct, UpdateProxyMode::Custom] {
            let proxy = UpdateProxy {
                mode,
                url: " http://127.0.0.1:7890 ".to_string(),
            };
            let actual = super::proxy_url(&proxy, |_| panic!("must ignore environment proxies"));
            assert_eq!(
                actual,
                (mode == UpdateProxyMode::Custom).then(|| proxy.url.trim().to_string())
            );
        }
    }

    #[test]
    fn update_proxy_addresses_are_validated_before_saving() {
        for url in [
            "http://127.0.0.1:7890",
            "http://localhost:8080/",
            "http://[::1]:7890",
        ] {
            assert!(super::parse_update_proxy("custom", url).is_ok(), "{url}");
        }
        for url in [
            "",
            "localhost:7890",
            "socks5://127.0.0.1:1080",
            "https://localhost:7890",
            "http://localhost:bad",
            "http://localhost:99999",
            "http://localhost:8080/path",
            "http://localhost:8080?token=secret",
        ] {
            assert_eq!(
                super::parse_update_proxy("custom", url)
                    .unwrap_err()
                    .to_string(),
                "Invalid HTTP proxy address",
                "{url}"
            );
        }
        assert!(super::parse_update_proxy("unknown", "").is_err());
        assert_eq!(
            super::parse_update_proxy("custom", " http://localhost:7890 ")
                .unwrap()
                .url,
            " http://localhost:7890 "
        );
        assert!(super::parse_update_proxy("direct", "").is_ok());
    }

    #[test]
    fn checks_and_asset_downloads_use_the_new_proxy_on_each_operation() {
        use std::{
            io::{BufRead, BufReader},
            net::TcpListener,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        for download in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let (sent, received) = mpsc::channel();
            let server = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = String::new();
                            BufReader::new(&mut stream).read_line(&mut request).unwrap();
                            sent.send(request).unwrap();
                            stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                            return;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "proxy was not contacted");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept proxy connection: {error}"),
                    }
                }
            });
            let proxy = super::parse_update_proxy("custom", &url).unwrap();
            if download {
                let client = super::build_github_client(true, &proxy).unwrap();
                assert!(
                    super::download_asset_bytes(&client, &asset(1, "SHA256SUMS", 1), 64).is_err()
                );
            } else {
                assert!(super::check_for_update(&proxy).is_err());
            }
            server.join().unwrap();
            assert_eq!(
                received.recv_timeout(Duration::from_secs(1)).unwrap(),
                "CONNECT api.github.com:443 HTTP/1.1\r\n"
            );
        }
    }

    #[test]
    fn newer_release_is_available() {
        assert!(matches!(
            evaluate_release("0.1.0", &release("v0.2.0", Vec::new())).unwrap(),
            UpdateCheck::Available { release } if release.version == "0.2.0".parse().unwrap()
        ));
    }

    #[test]
    fn equal_or_older_release_is_up_to_date() {
        assert!(matches!(
            evaluate_release("0.2.0", &release("0.2.0", Vec::new())).unwrap(),
            UpdateCheck::UpToDate { .. }
        ));
        assert!(matches!(
            evaluate_release("0.2.0", &release("v0.1.9", Vec::new())).unwrap(),
            UpdateCheck::UpToDate { .. }
        ));
    }

    #[test]
    fn prerelease_tags_follow_semver_ordering() {
        assert!(matches!(
            evaluate_release("0.1.0", &release("V0.2.0-beta.1", Vec::new())).unwrap(),
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

    #[test]
    fn checksum_manifest_requires_an_exact_asset_name() {
        let checksum = "a".repeat(64);
        let manifest = format!("{checksum}  spotlit_0.2.0_amd64.deb\n");

        assert_eq!(
            checksum_for_asset(&manifest, "spotlit_0.2.0_amd64.deb").unwrap(),
            checksum
        );
        assert!(checksum_for_asset(&manifest, "spotlit_0.2.0_amd64.deb.part").is_err());
    }

    #[test]
    fn downloaded_asset_must_match_the_github_digest() {
        let bytes = b"verified checksums";
        let digest = bytes_to_lower_hex(Sha256::digest(bytes).as_ref());
        let mut checksums = asset(1, "SHA256SUMS", bytes.len() as u64);
        checksums.digest = Some(format!("sha256:{digest}"));

        verify_downloaded_asset_digest(&checksums, bytes).unwrap();
        assert!(verify_downloaded_asset_digest(&checksums, b"changed").is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_download_requires_the_deb_and_checksum_assets() {
        let github = release(
            "0.2.0",
            vec![
                asset(1, "spotlit_0.2.0_amd64.deb", 8 * 1024 * 1024),
                asset(2, "SHA256SUMS", 256),
            ],
        );
        let UpdateCheck::Available { release } = evaluate_release("0.1.0", &github).unwrap() else {
            panic!("expected available release");
        };

        let spec = download_spec(&release, PackageTarget::LinuxDeb).unwrap();

        assert_eq!(spec.package.name, "spotlit_0.2.0_amd64.deb");
        assert_eq!(spec.checksums.name, "SHA256SUMS");
    }

    #[test]
    fn automatic_checks_are_limited_across_processes() {
        let root = temp_root("update-check-cadence");
        fs::create_dir_all(&root).unwrap();
        save_cache(
            &root,
            &UpdateCache {
                last_checked_unix: Some(1_000),
                release: None,
                ..UpdateCache::default()
            },
        )
        .unwrap();

        assert!(!automatic_check_due_at(&root, 1_000 + 60).unwrap());
        assert!(automatic_check_due_at(&root, 1_000 + 24 * 60 * 60).unwrap());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_check_replaces_a_malformed_cache() {
        let root = temp_root("malformed-update-cache");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(super::UPDATE_STATE_FILE), b"not JSON").unwrap();

        record_check_result(&root, &Ok(UpdateCheck::NoRelease)).unwrap();

        let cache = load_cache(&root).unwrap().unwrap();
        assert!(cache.last_checked_unix.is_some());
        assert!(cache.release.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_checks_retry_without_overwriting_the_last_successful_release() {
        let directory = TestDirectory::new("update-retry");
        let check = evaluate_release("0.1.0", &release("0.2.0", Vec::new())).unwrap();
        record_check_result_at(&directory.0, &Ok(check), 1_000).unwrap();
        record_check_result_at(&directory.0, &Err(anyhow::anyhow!("offline")), 2_000).unwrap();
        let cache = load_cache(&directory.0).unwrap().unwrap();
        assert_eq!(cache.last_checked_unix, Some(1_000));
        assert_eq!(cache.release.unwrap().tag_name, "0.2.0");
        assert!(!automatic_check_due_at(&directory.0, 2_001).unwrap());
        assert!(automatic_check_due_at(&directory.0, 2_000 + 15 * 60).unwrap());
        record_check_result_at(&directory.0, &Ok(UpdateCheck::NoRelease), 3_000).unwrap();
        assert!(!automatic_check_due_at(&directory.0, 3_000 + 15 * 60).unwrap());
        assert!(automatic_check_due_at(&directory.0, 3_000 + 24 * 60 * 60).unwrap());
    }

    #[test]
    fn first_failed_check_is_not_cached_as_no_release() {
        let directory = TestDirectory::new("first-update-failure");
        record_check_result_at(&directory.0, &Err(anyhow::anyhow!("offline")), 1_000).unwrap();
        assert!(super::cached_update_check(&directory.0).unwrap().is_none());
        assert!(automatic_check_due_at(&directory.0, 1_000 + 15 * 60).unwrap());
    }

    #[test]
    fn backwards_clock_does_not_suspend_update_checks() {
        let directory = TestDirectory::new("update-clock");
        record_check_result_at(&directory.0, &Ok(UpdateCheck::NoRelease), 2_000).unwrap();
        assert!(automatic_check_due_at(&directory.0, 1_000).unwrap());
    }

    #[test]
    fn github_windows_archive_layout_matches_download_and_extraction() {
        let directory = TestDirectory::new("windows-update-archive");
        let github = release(
            "0.2.0",
            vec![
                asset(1, "spotlit-0.2.0-x86_64-pc-windows-msvc.zip", 1_024),
                asset(2, "SHA256SUMS", 256),
            ],
        );
        let UpdateCheck::Available { release } = evaluate_release("0.1.0", &github).unwrap() else {
            panic!("expected update");
        };
        let spec = download_spec(&release, PackageTarget::WindowsZip).unwrap();
        let root = expected_windows_archive_root(&release);
        assert_eq!(root, "spotlit-0.2.0");
        let archive = directory.0.join(&spec.package.name);
        let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
        for name in [
            "spotlit.exe",
            "README.md",
            "LICENSE",
            "THIRD-PARTY-LICENSES.txt",
        ] {
            zip.start_file(
                format!("spotlit-0.2.0/{name}"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(b"fixture").unwrap();
        }
        zip.finish().unwrap();
        let staged = directory.0.join("staged");
        extract_windows_package(&archive, &root, &staged).unwrap();
        assert_eq!(fs::read(staged.join("spotlit.exe")).unwrap(), b"fixture");
        assert!(staged.join("THIRD-PARTY-LICENSES.txt").is_file());
    }

    #[test]
    fn windows_update_rejects_wrong_roots_traversal_and_unexpected_files() {
        let directory = TestDirectory::new("unsafe-update-archive");
        let root = "spotlit-0.2.0";
        for entry in [
            "wrong-root/spotlit.exe".to_string(),
            format!("{root}/../spotlit.exe"),
            format!("{root}/unexpected.exe"),
        ] {
            let archive = directory.0.join("update.zip");
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            zip.start_file(entry, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"fixture").unwrap();
            zip.finish().unwrap();
            assert!(extract_windows_package(&archive, root, &directory.0.join("staged")).is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a release archive built by the release workflow"]
    fn windows_release_archive_matches_updater() {
        let archive = PathBuf::from(
            std::env::var_os("SPOTLIT_TEST_WINDOWS_UPDATE_ARCHIVE").expect("release archive path"),
        );
        let github = release(env!("CARGO_PKG_VERSION"), Vec::new());
        let UpdateCheck::Available { release } = evaluate_release("0.0.0", &github).unwrap() else {
            panic!("expected update");
        };
        let directory = TestDirectory::new("real-windows-update-archive");
        extract_windows_package(
            &archive,
            &expected_windows_archive_root(&release),
            &directory.0.join("staged"),
        )
        .unwrap();
        assert!(
            fs::metadata(directory.0.join("staged/spotlit.exe"))
                .unwrap()
                .len()
                > 0
        );
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = temp_root(label);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    mod debian {
        use std::{
            collections::VecDeque,
            os::unix::process::ExitStatusExt,
            path::Path,
            process::{Command, ExitStatus, Output},
        };

        use super::{TestDirectory, bytes_to_lower_hex, fs};
        use crate::update::{
            InstallDisposition, PreparedUpdate, install_debian_update_with,
            supports_debian_installation,
        };
        use sha2::{Digest, Sha256};

        fn output(code: i32, stdout: &str) -> Output {
            Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: b"fixture diagnostic".to_vec(),
            }
        }

        fn prepared(directory: &TestDirectory) -> PreparedUpdate {
            let package_path = directory.0.join("spotlit_0.2.0_amd64.deb");
            if !package_path.exists() {
                fs::write(&package_path, b"fixture package").unwrap();
            }
            let bytes = fs::read(&package_path).unwrap();
            PreparedUpdate {
                version: "0.2.0".parse().unwrap(),
                package_path,
                size: bytes.len() as u64,
                sha256: bytes_to_lower_hex(Sha256::digest(bytes).as_ref()),
            }
        }

        fn simulate(
            update: &PreparedUpdate,
            responses: Vec<Output>,
        ) -> anyhow::Result<InstallDisposition> {
            let mut responses = VecDeque::from(responses);
            let mut calls = 0;
            let result = install_debian_update_with(update, |command| {
                let programs = [
                    "/usr/bin/dpkg-deb",
                    "/usr/bin/dpkg-query",
                    "/usr/bin/pkexec",
                    "/usr/bin/dpkg-query",
                ];
                assert_eq!(command.get_program(), programs[calls]);
                if calls == 1 {
                    calls += 1;
                    return Ok(output(0, "spotlit\n0.1.0\namd64\ninstalled"));
                }
                if calls == 2 {
                    let args = command.get_args().collect::<Vec<_>>();
                    assert_eq!(
                        &args[..6],
                        [
                            "--disable-internal-agent",
                            "/usr/bin/apt-get",
                            "--assume-yes",
                            "--no-install-recommends",
                            "--no-remove",
                            "install"
                        ]
                    );
                    assert_eq!(args[6], update.package_path.as_os_str());
                }
                calls += 1;
                Ok(responses.pop_front().expect("unexpected system command"))
            });
            assert!(responses.is_empty(), "expected command was not reached");
            result
        }

        #[test]
        fn portable_and_development_builds_never_install_debian_updates() {
            for distribution in [
                None,
                Some("github-linux-portable"),
                Some("github-windows-portable"),
            ] {
                assert!(!supports_debian_installation(
                    distribution,
                    Path::new("/usr/bin/spotlit")
                ));
            }
            assert!(!supports_debian_installation(
                Some("github-linux-deb"),
                Path::new("/home/user/spotlit/spotlit")
            ));
            assert!(supports_debian_installation(
                Some("github-linux-deb"),
                Path::new("/usr/bin/spotlit")
            ));
            assert!(supports_debian_installation(
                Some("github-linux-deb"),
                Path::new("/usr/bin/spotlit (deleted)")
            ));
        }

        #[test]
        fn installation_success_requires_a_confirmed_installed_version() {
            let directory = TestDirectory::new("debian-update space");
            let update = prepared(&directory);
            assert_eq!(
                simulate(
                    &update,
                    vec![
                        output(0, "spotlit\n0.2.0\namd64"),
                        output(0, ""),
                        output(0, "spotlit\n0.2.0\namd64\ninstalled")
                    ]
                )
                .unwrap(),
                InstallDisposition::Installed
            );
            assert!(
                simulate(
                    &update,
                    vec![
                        output(0, "spotlit\n0.2.0\namd64"),
                        output(0, ""),
                        output(0, "spotlit\n0.1.0\namd64\ninstalled")
                    ]
                )
                .is_err()
            );
        }

        #[test]
        fn authorization_cancel_and_installation_errors_are_distinct() {
            let directory = TestDirectory::new("debian-cancel");
            let update = prepared(&directory);
            assert_eq!(
                simulate(
                    &update,
                    vec![output(0, "spotlit\n0.2.0\namd64"), output(126, "")]
                )
                .unwrap(),
                InstallDisposition::Cancelled
            );
            for code in [127, 100] {
                let error = simulate(
                    &update,
                    vec![output(0, "spotlit\n0.2.0\namd64"), output(code, "")],
                )
                .unwrap_err();
                assert!(error.to_string().contains("fixture diagnostic"));
            }
        }

        #[test]
        fn wrong_package_identity_never_reaches_authorization() {
            let directory = TestDirectory::new("debian-identity");
            let update = prepared(&directory);
            for identity in [
                "other-app\n0.2.0\namd64",
                "spotlit\n0.1.0\namd64",
                "spotlit\n0.2.0\narm64",
            ] {
                assert!(simulate(&update, vec![output(0, identity)]).is_err());
            }
        }

        #[test]
        fn missing_or_changed_packages_never_run_system_commands() {
            let directory = TestDirectory::new("debian-integrity");
            let update = prepared(&directory);
            let mut changed = fs::read(&update.package_path).unwrap();
            changed[0] ^= 1;
            fs::write(&update.package_path, changed).unwrap();
            assert!(simulate(&update, Vec::new()).is_err());
            fs::remove_file(&update.package_path).unwrap();
            assert!(simulate(&update, Vec::new()).is_err());
        }

        #[test]
        fn real_debian_metadata_is_accepted_without_installing_the_fixture() {
            let directory = TestDirectory::new("real-debian-metadata");
            let package_root = directory.0.join("package");
            fs::create_dir_all(package_root.join("DEBIAN")).unwrap();
            fs::write(package_root.join("DEBIAN/control"), "Package: spotlit\nVersion: 0.2.0\nArchitecture: amd64\nMaintainer: Test <test@example.invalid>\nDescription: updater test fixture\n").unwrap();
            let package = directory.0.join("spotlit_0.2.0_amd64.deb");
            let built = Command::new("dpkg-deb")
                .arg("--build")
                .arg(package_root)
                .arg(package)
                .output()
                .unwrap();
            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
            let update = prepared(&directory);
            let mut calls = 0;
            let result = install_debian_update_with(&update, |command| {
                calls += 1;
                match command.get_program().to_str().unwrap() {
                    "/usr/bin/dpkg-deb" => command.output(),
                    "/usr/bin/pkexec" => Ok(output(0, "")),
                    "/usr/bin/dpkg-query" => Ok(output(
                        0,
                        if calls == 2 {
                            "spotlit\n0.1.0\namd64\ninstalled"
                        } else {
                            "spotlit\n0.2.0\namd64\ninstalled"
                        },
                    )),
                    other => panic!("unexpected command: {other}"),
                }
            })
            .unwrap();
            assert_eq!(result, InstallDisposition::Installed);
            assert_eq!(calls, 4);
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spotlit-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
