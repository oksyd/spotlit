use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
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

const GITHUB_API_HOST: &str = "https://api.github.com";
const LATEST_RELEASE_PATH: &str = "/repos/oksyd/spotlit/releases/latest";
const RELEASE_ASSET_PATH_PREFIX: &str = "/repos/oksyd/spotlit/releases/assets/";
const GITHUB_API_VERSION: &str = "2026-03-10";
const RELEASE_METADATA_MAX_BYTES: usize = 64 * 1024;
const CHECKSUMS_MAX_BYTES: usize = 64 * 1024;
const PACKAGE_MAX_BYTES: u64 = 128 * 1024 * 1024;
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
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

static GITHUB_CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();
static GITHUB_DOWNLOAD_CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();

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
    #[cfg(windows)]
    archive_root: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InstallDisposition {
    #[cfg(target_os = "linux")]
    ExternalInstaller,
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
    release: Option<GitHubRelease>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PackageTarget {
    #[cfg(target_os = "linux")]
    LinuxDeb,
    #[cfg(windows)]
    WindowsZip,
}

struct DownloadSpec<'a> {
    package: &'a ReleaseAsset,
    checksums: &'a ReleaseAsset,
    #[cfg(windows)]
    archive_root: String,
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

    evaluate_release(env!("CARGO_PKG_VERSION"), &release)
}

pub(crate) fn install_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        DISTRIBUTION == Some("github-linux-deb")
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
        return Ok(Some(UpdateCheck::NoRelease));
    };
    evaluate_release(env!("CARGO_PKG_VERSION"), &release).map(Some)
}

pub(crate) fn record_check_result(update_dir: &Path, result: &Result<UpdateCheck>) -> Result<()> {
    let mut cache = load_cache(update_dir).ok().flatten().unwrap_or_default();
    cache.last_checked_unix = Some(unix_now());
    if let Ok(check) = result {
        cache.release = check.release().map(GitHubRelease::from);
    }
    save_cache(update_dir, &cache)
}

pub(crate) fn download_update(release: &ReleaseInfo, update_dir: &Path) -> Result<PreparedUpdate> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    let spec = download_spec(release, current_package_target())?;
    fs::create_dir_all(update_dir)
        .with_context(|| format!("create Spotlit update directory {}", update_dir.display()))?;

    let checksum_bytes = download_asset_bytes(spec.checksums, CHECKSUMS_MAX_BYTES)
        .context("download release checksums")?;
    verify_downloaded_asset_digest(spec.checksums, &checksum_bytes)?;
    let checksum_text =
        std::str::from_utf8(&checksum_bytes).context("release checksums are not valid UTF-8")?;
    let expected_sha256 = checksum_for_asset(checksum_text, &spec.package.name)?;
    verify_api_digest(spec.package, &expected_sha256)?;

    let package_path = update_dir.join(&spec.package.name);
    if !existing_package_is_valid(&package_path, spec.package.size, &expected_sha256)? {
        download_package(spec.package, &package_path, &expected_sha256)?;
    }

    Ok(PreparedUpdate {
        version: release.version.clone(),
        package_path,
        #[cfg(windows)]
        archive_root: spec.archive_root,
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn install_prepared_update(update: &PreparedUpdate) -> Result<InstallDisposition> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    ensure!(
        update
            .package_path
            .extension()
            .is_some_and(|value| value == "deb"),
        "the prepared update is not a Debian package"
    );
    ensure!(
        update.package_path.is_file(),
        "the prepared update no longer exists"
    );

    Command::new("xdg-open")
        .arg(&update.package_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "open {} with the system installer",
                update.package_path.display()
            )
        })?;
    Ok(InstallDisposition::ExternalInstaller)
}

#[cfg(windows)]
pub(crate) fn install_prepared_update(update: &PreparedUpdate) -> Result<InstallDisposition> {
    ensure!(
        install_supported(),
        "this Spotlit build cannot install updates"
    );
    let current_exe = env::current_exe().context("resolve current Spotlit executable")?;
    let update_dir = update
        .package_path
        .parent()
        .context("prepared update has no parent directory")?;
    let staged_dir = update_dir.join(format!("staged-{}", update.version));
    extract_windows_package(update, &staged_dir)?;

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

    Ok(DownloadSpec {
        package,
        checksums,
        #[cfg(windows)]
        archive_root: expected_windows_archive_root(release),
    })
}

fn expected_package_name(release: &ReleaseInfo, target: PackageTarget) -> String {
    match target {
        #[cfg(target_os = "linux")]
        PackageTarget::LinuxDeb => format!("spotlit_{}_amd64.deb", release.version),
        #[cfg(windows)]
        PackageTarget::WindowsZip => format!(
            "{}-x86_64-pc-windows-msvc.zip",
            expected_windows_archive_root(release)
        ),
    }
}

#[cfg(windows)]
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

fn download_asset_bytes(asset: &ReleaseAsset, max_bytes: usize) -> Result<Vec<u8>> {
    ensure!(asset.size <= max_bytes as u64, "release asset is too large");
    let response = github_download_client()?
        .get(format!("{RELEASE_ASSET_PATH_PREFIX}{}", asset.id))
        .try_header("User-Agent", USER_AGENT)
        .context("build GitHub asset request")?
        .try_header("Accept", "application/octet-stream")
        .context("build GitHub asset request")?
        .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .context("build GitHub asset request")?
        .send_response_stream()
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

fn download_package(asset: &ReleaseAsset, destination: &Path, expected_sha256: &str) -> Result<()> {
    let part_path = destination.with_extension(format!(
        "{}.part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("download")
    ));
    remove_file_if_exists(&part_path)?;

    let result = (|| {
        let mut response = github_download_client()?
            .get(format!("{RELEASE_ASSET_PATH_PREFIX}{}", asset.id))
            .try_header("User-Agent", USER_AGENT)
            .context("build GitHub package request")?
            .try_header("Accept", "application/octet-stream")
            .context("build GitHub package request")?
            .try_header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .context("build GitHub package request")?
            .send_response_stream()
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
    let Some(last_checked) = cache.last_checked_unix else {
        return Ok(true);
    };
    Ok(now.saturating_sub(last_checked) >= UPDATE_CHECK_INTERVAL.as_secs())
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

fn github_client() -> Result<&'static Client> {
    GITHUB_CLIENT
        .get_or_init(|| build_github_client(false))
        .as_ref()
        .map_err(|error| anyhow!("initialize GitHub HTTP client: {error}"))
}

fn github_download_client() -> Result<&'static Client> {
    GITHUB_DOWNLOAD_CLIENT
        .get_or_init(|| build_github_client(true))
        .as_ref()
        .map_err(|error| anyhow!("initialize GitHub download client: {error}"))
}

fn build_github_client(download: bool) -> std::result::Result<Client, String> {
    let builder = Client::builder(GITHUB_API_HOST)
        .client_name(if download {
            "spotlit-update-download"
        } else {
            "spotlit-update-check"
        })
        .request_timeout(Duration::from_secs(if download { 30 } else { 10 }))
        .total_timeout(Duration::from_secs(if download { 5 * 60 } else { 20 }))
        .connect_timeout(Duration::from_secs(8))
        .redirect_policy(RedirectPolicy::limited(5))
        .max_response_body_bytes(RELEASE_METADATA_MAX_BYTES);

    configure_proxy(builder)?
        .build()
        .map_err(|error| error.to_string())
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

#[cfg(windows)]
fn extract_windows_package(update: &PreparedUpdate, staged_dir: &Path) -> Result<()> {
    use zip::ZipArchive;

    if staged_dir.exists() {
        fs::remove_dir_all(staged_dir)
            .with_context(|| format!("remove stale update staging {}", staged_dir.display()))?;
    }
    fs::create_dir_all(staged_dir)
        .with_context(|| format!("create update staging {}", staged_dir.display()))?;

    let file = File::open(&update.package_path)
        .with_context(|| format!("open update archive {}", update.package_path.display()))?;
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
            .strip_prefix(&update.archive_root)
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
    use std::{fs, path::PathBuf};

    use super::{
        GitHubRelease, PackageTarget, ReleaseAsset, UpdateCache, UpdateCheck,
        automatic_check_due_at, bytes_to_lower_hex, checksum_for_asset, download_spec,
        evaluate_release, load_cache, parse_release_tag, record_check_result,
        release_response_has_body, save_cache, verify_downloaded_asset_digest,
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

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "spotlit-{label}-{}-{}",
            std::process::id(),
            super::unix_now()
        ))
    }
}
