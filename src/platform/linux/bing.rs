use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use crate::core::{AppPaths, DesktopSpotlightCreative, Result, SpotlightMetadata, SpotlitError};
use reqx::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};

const BING_HOST: &str = "https://www.bing.com";
const BING_METADATA_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const BING_METADATA_MAX_BYTES: usize = 256 * 1024;
const BING_IMAGE_MAX_BYTES: usize = 32 * 1024 * 1024;
const BING_ARCHIVE_IMAGE_COUNT: usize = 7;
const BING_MARKETS: &[&str] = &["en-US"];
const BING_PROXY_ENV_KEYS: &[&str] = &["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
const FALLBACK_BING_TITLE: &str = "Bing Wallpaper";
const USER_AGENT: &str = "Spotlit/0.1 (+https://github.com/oksyd/spotlit)";

static BING_CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();

pub fn bing_wallpaper_dir(paths: &AppPaths) -> PathBuf {
    paths.data_dir.join("bing")
}

pub fn refresh_bing_wallpapers(cache_dir: &Path) -> Result<Vec<DesktopSpotlightCreative>> {
    fs::create_dir_all(cache_dir).map_err(|source| SpotlitError::io(cache_dir, source))?;

    let mut creatives = Vec::new();
    for &market in BING_MARKETS {
        let metadata_path = market_metadata_path(cache_dir, market);

        match fresh_cached_creatives(cache_dir, &metadata_path, market) {
            Ok(Some(mut market_creatives)) => {
                creatives.append(&mut market_creatives);
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, market, "failed to use cached Bing metadata");
            }
        }

        match fetch_latest_bing_archive(market).and_then(|archive| {
            let creatives = cache_archive_images(cache_dir, &archive, market)?;
            write_archive_metadata(&metadata_path, &archive)?;
            Ok(creatives)
        }) {
            Ok(mut market_creatives) => creatives.append(&mut market_creatives),
            Err(error) => {
                tracing::warn!(%error, market, "failed to refresh Bing wallpaper");
                creatives.append(&mut cached_creatives(cache_dir, &metadata_path, market)?);
            }
        }
    }

    Ok(creatives)
}

fn market_metadata_path(cache_dir: &Path, market: &str) -> PathBuf {
    cache_dir.join(format!("latest-{}.json", sanitize_token(market)))
}

fn fresh_cached_creatives(
    cache_dir: &Path,
    metadata_path: &Path,
    market: &str,
) -> Result<Option<Vec<DesktopSpotlightCreative>>> {
    let Some(modified_at) = metadata_path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
    else {
        return Ok(None);
    };

    let fresh = modified_at
        .elapsed()
        .map(|elapsed| elapsed <= BING_METADATA_MAX_AGE)
        .unwrap_or(false);
    if !fresh {
        return Ok(None);
    }

    let Some(archive) = read_cached_archive(metadata_path)? else {
        return Ok(None);
    };
    if archive.images.len() < BING_ARCHIVE_IMAGE_COUNT {
        return Ok(None);
    }

    cache_archive_images(cache_dir, &archive, market).map(Some)
}

fn cached_creatives(
    cache_dir: &Path,
    metadata_path: &Path,
    market: &str,
) -> Result<Vec<DesktopSpotlightCreative>> {
    let Some(archive) = read_cached_archive(metadata_path)? else {
        return Ok(Vec::new());
    };

    Ok(existing_creatives_from_archive(cache_dir, &archive, market))
}

fn read_cached_archive(metadata_path: &Path) -> Result<Option<BingArchive>> {
    match fs::read_to_string(metadata_path) {
        Ok(contents) => serde_json::from_str::<BingArchive>(&contents)
            .map_err(|source| {
                SpotlitError::platform(format!("parse cached Bing metadata: {source}"))
            })
            .map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SpotlitError::io(metadata_path, source)),
    }
}

fn fetch_latest_bing_archive(market: &str) -> Result<BingArchive> {
    bing_client()?
        .get("/HPImageArchive.aspx")
        .query_pair("format", "js")
        .query_pair("idx", "0")
        .query_pair("n", BING_ARCHIVE_IMAGE_COUNT.to_string())
        .query_pair("mkt", market)
        .try_header("User-Agent", USER_AGENT)
        .map_err(|source| SpotlitError::platform(format!("build Bing metadata request: {source}")))?
        .try_header("Accept-Language", ACCEPT_LANGUAGE)
        .map_err(|source| SpotlitError::platform(format!("build Bing metadata request: {source}")))?
        .max_response_body_bytes(BING_METADATA_MAX_BYTES)
        .send_json::<BingArchive>()
        .map_err(|source| SpotlitError::platform(format!("parse Bing metadata: {source}")))
}

fn cache_archive_images(
    cache_dir: &Path,
    archive: &BingArchive,
    market: &str,
) -> Result<Vec<DesktopSpotlightCreative>> {
    if archive.images.is_empty() {
        return Err(SpotlitError::platform(
            "Bing metadata did not include an image",
        ));
    }

    let mut creatives = Vec::with_capacity(archive.images.len());
    for (index, image) in archive.images.iter().enumerate() {
        match cache_archive_image(cache_dir, image, market, index == 0) {
            Ok(creative) => creatives.push(creative),
            Err(error) => {
                tracing::warn!(%error, market, image_index = index, "failed to cache Bing image");
            }
        }
    }

    Ok(creatives)
}

fn existing_creatives_from_archive(
    cache_dir: &Path,
    archive: &BingArchive,
    market: &str,
) -> Vec<DesktopSpotlightCreative> {
    archive
        .images
        .iter()
        .enumerate()
        .filter_map(|(index, image)| {
            let path = image_path(cache_dir, image, market);
            path.exists()
                .then(|| creative_from_image(path, image, index == 0))
        })
        .collect()
}

fn cache_archive_image(
    cache_dir: &Path,
    image: &BingImage,
    market: &str,
    is_current: bool,
) -> Result<DesktopSpotlightCreative> {
    let path = image_path(cache_dir, image, market);

    if !path.exists() {
        download_image(&image.url, &path)?;
    }

    Ok(creative_from_image(path, image, is_current))
}

fn download_image(url: &str, path: &Path) -> Result<()> {
    let tmp_path = path.with_extension("download");

    {
        let mut file =
            fs::File::create(&tmp_path).map_err(|source| SpotlitError::io(&tmp_path, source))?;
        bing_client()?
            .get(bing_request_path(url))
            .try_header("User-Agent", USER_AGENT)
            .map_err(|source| {
                SpotlitError::platform(format!("build Bing image request: {source}"))
            })?
            .download_to_writer_limited(&mut file, BING_IMAGE_MAX_BYTES)
            .map_err(|source| SpotlitError::platform(format!("download Bing image: {source}")))?;
        file.flush()
            .map_err(|source| SpotlitError::io(&tmp_path, source))?;
    }

    fs::rename(&tmp_path, path).map_err(|source| SpotlitError::io(path, source))?;
    Ok(())
}

fn bing_client() -> Result<&'static Client> {
    BING_CLIENT
        .get_or_init(|| {
            let builder = Client::builder(BING_HOST)
                .client_name("spotlit")
                .request_timeout(Duration::from_secs(20))
                .total_timeout(Duration::from_secs(45))
                .connect_timeout(Duration::from_secs(10))
                .max_response_body_bytes(BING_METADATA_MAX_BYTES);

            configure_bing_proxy(builder)?
                .build()
                .map_err(|source| source.to_string())
        })
        .as_ref()
        .map_err(|error| SpotlitError::platform(format!("initialize Bing HTTP client: {error}")))
}

fn configure_bing_proxy(builder: ClientBuilder) -> std::result::Result<ClientBuilder, String> {
    let Some(proxy_url) = bing_proxy_url_from_env() else {
        return Ok(builder);
    };

    let proxy_uri = proxy_url
        .parse()
        .map_err(|source| format!("parse Bing proxy URI from environment: {source}"))?;
    Ok(builder.http_proxy(proxy_uri))
}

fn bing_proxy_url_from_env() -> Option<String> {
    first_non_empty_value(
        BING_PROXY_ENV_KEYS
            .iter()
            .filter_map(|name| env::var(name).ok()),
    )
}

fn first_non_empty_value(values: impl IntoIterator<Item = String>) -> Option<String> {
    values.into_iter().find_map(non_empty)
}

fn bing_request_path(value: &str) -> String {
    if let Some(path) = value.strip_prefix(BING_HOST) {
        path.to_string()
    } else if value.starts_with('/') {
        value.to_string()
    } else {
        format!("/{value}")
    }
}

fn write_archive_metadata(path: &Path, archive: &BingArchive) -> Result<()> {
    let tmp_path = path.with_extension("tmp");
    let serialized = serde_json::to_vec_pretty(archive)
        .map_err(|source| SpotlitError::platform(format!("serialize Bing metadata: {source}")))?;
    fs::write(&tmp_path, serialized).map_err(|source| SpotlitError::io(&tmp_path, source))?;
    fs::rename(&tmp_path, path).map_err(|source| SpotlitError::io(path, source))
}

fn creative_from_image(
    path: PathBuf,
    image: &BingImage,
    is_current: bool,
) -> DesktopSpotlightCreative {
    DesktopSpotlightCreative {
        landscape_path: path,
        portrait_path: None,
        metadata: metadata_from_image(image),
        is_current,
    }
}

fn metadata_from_image(image: &BingImage) -> SpotlightMetadata {
    let localized_to_chinese = image_looks_chinese_localized(image);
    let title = english_visible_text(image.title.clone())
        .filter(|_| !localized_to_chinese)
        .or_else(|| fallback_title_from_image(image));
    let copyright = english_visible_text(image.copyright.clone()).filter(|_| !localized_to_chinese);
    let caption = english_visible_text(image.title.clone())
        .filter(|_| !localized_to_chinese)
        .or_else(|| copyright.clone());
    let info_url = image
        .copyrightlink
        .as_deref()
        .filter(|link| !localized_to_chinese && !has_chinese_locale_marker(link))
        .map(absolute_bing_url);

    SpotlightMetadata {
        spotlight_id: image
            .hsh
            .clone()
            .and_then(non_empty)
            .or_else(|| image.startdate.clone().and_then(non_empty)),
        title,
        caption,
        copyright,
        info_url,
        content_id: image.urlbase.clone().and_then(non_empty),
    }
    .normalized()
}

fn image_path(cache_dir: &Path, image: &BingImage, market: &str) -> PathBuf {
    let date = image.startdate.as_deref().unwrap_or("latest");
    let id = image
        .hsh
        .as_deref()
        .or(image.urlbase.as_deref())
        .unwrap_or("bing");
    let extension = image_extension(&image.url).unwrap_or("jpg");
    cache_dir.join(format!(
        "bing-{}-{}-{}.{}",
        sanitize_token(market),
        sanitize_token(date),
        sanitize_token(id),
        extension
    ))
}

fn image_extension(url: &str) -> Option<&'static str> {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".png") {
        Some("png")
    } else if path.ends_with(".webp") {
        Some("webp")
    } else if path.ends_with(".jpeg") || path.ends_with(".jpg") {
        Some("jpg")
    } else {
        None
    }
}

fn absolute_bing_url(value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else if value.starts_with('/') {
        format!("{BING_HOST}{value}")
    } else {
        format!("{BING_HOST}/{value}")
    }
}

fn sanitize_token(value: &str) -> String {
    let mut token = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            token.push(character);
        } else if !token.ends_with('-') {
            token.push('-');
        }
    }

    token.trim_matches('-').to_string()
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn english_visible_text(value: Option<String>) -> Option<String> {
    value
        .and_then(non_empty)
        .filter(|value| !contains_cjk_text(value))
}

fn image_looks_chinese_localized(image: &BingImage) -> bool {
    [
        Some(image.url.as_str()),
        image.urlbase.as_deref(),
        image.copyrightlink.as_deref(),
        image.title.as_deref(),
        image.copyright.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| has_chinese_locale_marker(value) || contains_cjk_text(value))
}

fn has_chinese_locale_marker(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("zh-cn")
        || value.contains("zh-hans")
        || value.contains("zh-hant")
        || value.contains("zh-tw")
        || value.contains("zh-hk")
}

fn contains_cjk_text(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character,
            '\u{3400}'..='\u{4DBF}'
                | '\u{4E00}'..='\u{9FFF}'
                | '\u{F900}'..='\u{FAFF}'
        )
    })
}

fn fallback_title_from_image(image: &BingImage) -> Option<String> {
    image
        .urlbase
        .as_deref()
        .and_then(ohr_slug_title)
        .or_else(|| ohr_slug_title(&image.url))
        .or_else(|| Some(FALLBACK_BING_TITLE.to_string()))
}

fn ohr_slug_title(value: &str) -> Option<String> {
    let marker = "OHR.";
    let start = value.find(marker)? + marker.len();
    let slug: String = value[start..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect();
    if slug.is_empty() {
        return None;
    }

    Some(camel_slug_to_title(&slug))
}

fn camel_slug_to_title(slug: &str) -> String {
    let mut title = String::with_capacity(slug.len() + 8);
    let mut previous: Option<char> = None;
    let mut next = slug.chars().peekable();

    while let Some(character) = next.next() {
        if let Some(previous) = previous {
            let insert_space = character.is_ascii_uppercase()
                && (previous.is_ascii_lowercase()
                    || next.peek().is_some_and(|next| {
                        next.is_ascii_lowercase() && previous.is_ascii_uppercase()
                    }));
            if insert_space {
                title.push(' ');
            }
        }
        title.push(character);
        previous = Some(character);
    }

    title
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BingArchive {
    images: Vec<BingImage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BingImage {
    url: String,
    #[serde(default)]
    urlbase: Option<String>,
    #[serde(default)]
    startdate: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
    #[serde(default)]
    copyrightlink: Option<String>,
    #[serde(default)]
    hsh: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bing_markets_default_to_us_only() {
        assert_eq!(BING_MARKETS, &["en-US"]);
        assert_eq!(BING_ARCHIVE_IMAGE_COUNT, 7);
    }

    #[test]
    fn market_metadata_path_uses_market_token() {
        let cache_dir = Path::new("cache");

        assert_eq!(
            market_metadata_path(cache_dir, "en-US"),
            cache_dir.join("latest-en-US.json")
        );
    }

    #[test]
    fn first_non_empty_value_trims_and_skips_empty_values() {
        assert_eq!(
            first_non_empty_value(["", "  ", " http://127.0.0.1:7890 "].map(str::to_string)),
            Some("http://127.0.0.1:7890".to_string())
        );
    }

    #[test]
    fn cache_archive_images_reuses_existing_files_without_download()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let cache_dir = unique_test_dir("bing-existing");
        fs::create_dir_all(&cache_dir)?;
        let archive = BingArchive {
            images: vec![
                test_image(
                    "/th?id=OHR.SaguaroSun_EN-US8982109543_1920x1080.jpg&pid=hp",
                    "/th?id=OHR.SaguaroSun_EN-US8982109543",
                    "20260628",
                    "e5d162121cfb2b5ce71b9998a9db2836",
                ),
                test_image(
                    "/th?id=OHR.BoraBora_EN-US1234567890_1920x1080.jpg&pid=hp",
                    "/th?id=OHR.BoraBora_EN-US1234567890",
                    "20260627",
                    "70b8afa9d4f99eb628cd672f73f9b199",
                ),
            ],
        };

        for image in &archive.images {
            fs::write(image_path(&cache_dir, image, "en-US"), b"cached")?;
        }

        let creatives = cache_archive_images(&cache_dir, &archive, "en-US")?;

        assert_eq!(creatives.len(), 2);
        assert!(creatives[0].is_current);
        assert!(!creatives[1].is_current);
        assert!(creatives[0].landscape_path.exists());
        assert!(creatives[1].landscape_path.exists());

        fs::remove_dir_all(cache_dir)?;
        Ok(())
    }

    #[test]
    fn existing_creatives_from_archive_skips_missing_files()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let cache_dir = unique_test_dir("bing-existing-only");
        fs::create_dir_all(&cache_dir)?;
        let archive = BingArchive {
            images: vec![
                test_image(
                    "/th?id=OHR.SaguaroSun_EN-US8982109543_1920x1080.jpg&pid=hp",
                    "/th?id=OHR.SaguaroSun_EN-US8982109543",
                    "20260628",
                    "e5d162121cfb2b5ce71b9998a9db2836",
                ),
                test_image(
                    "/th?id=OHR.BoraBora_EN-US1234567890_1920x1080.jpg&pid=hp",
                    "/th?id=OHR.BoraBora_EN-US1234567890",
                    "20260627",
                    "70b8afa9d4f99eb628cd672f73f9b199",
                ),
            ],
        };
        fs::write(
            image_path(&cache_dir, &archive.images[1], "en-US"),
            b"cached",
        )?;

        let creatives = existing_creatives_from_archive(&cache_dir, &archive, "en-US");

        assert_eq!(creatives.len(), 1);
        assert!(!creatives[0].is_current);
        assert_eq!(
            creatives[0].metadata.spotlight_id.as_deref(),
            Some("70b8afa9d4f99eb628cd672f73f9b199")
        );

        fs::remove_dir_all(cache_dir)?;
        Ok(())
    }

    #[test]
    fn chinese_bing_metadata_uses_english_fallback_title() {
        let metadata = metadata_from_image(&BingImage {
            url: "/th?id=OHR.BoneyardBeach_ZH-CN5540590570_1920x1080.jpg&pid=hp".to_string(),
            urlbase: Some("/th?id=OHR.BoneyardBeach_ZH-CN5540590570".to_string()),
            startdate: Some("20260626".to_string()),
            title: Some("逐渐失去立足之地的树木".to_string()),
            copyright: Some("博尼亚德海滩上的漂流木, 亨廷岛, 南卡罗来纳州, 美国".to_string()),
            copyrightlink: Some(
                "https://www.bing.com/search?q=亨廷岛&form=hpcapt&mkt=zh-cn".to_string(),
            ),
            hsh: Some("430c1f4847a17f50aecd5df4f069b9b9".to_string()),
        });

        assert_eq!(metadata.title.as_deref(), Some("Boneyard Beach"));
        assert_eq!(metadata.caption, None);
        assert_eq!(metadata.copyright, None);
        assert_eq!(metadata.info_url, None);
        assert_eq!(
            metadata.spotlight_id.as_deref(),
            Some("430c1f4847a17f50aecd5df4f069b9b9")
        );
    }

    #[test]
    fn english_bing_metadata_is_preserved() {
        let metadata = metadata_from_image(&BingImage {
            url: "/th?id=OHR.BoneyardBeach_EN-US5540590570_1920x1080.jpg&pid=hp".to_string(),
            urlbase: Some("/th?id=OHR.BoneyardBeach_EN-US5540590570".to_string()),
            startdate: Some("20260626".to_string()),
            title: Some("Boneyard Beach".to_string()),
            copyright: Some(
                "Driftwood on Boneyard Beach, Hunting Island, South Carolina, United States"
                    .to_string(),
            ),
            copyrightlink: Some(
                "https://www.bing.com/search?q=Hunting+Island&form=hpcapt&mkt=en-us".to_string(),
            ),
            hsh: Some("430c1f4847a17f50aecd5df4f069b9b9".to_string()),
        });

        assert_eq!(metadata.title.as_deref(), Some("Boneyard Beach"));
        assert_eq!(metadata.caption.as_deref(), Some("Boneyard Beach"));
        assert_eq!(
            metadata.copyright.as_deref(),
            Some("Driftwood on Boneyard Beach, Hunting Island, South Carolina, United States")
        );
        assert_eq!(
            metadata.info_url.as_deref(),
            Some("https://www.bing.com/search?q=Hunting+Island&form=hpcapt&mkt=en-us")
        );
    }

    #[test]
    fn ohr_slug_title_splits_camel_case() {
        assert_eq!(
            ohr_slug_title("/th?id=OHR.RedSeaCoral_EN-US1234567890"),
            Some("Red Sea Coral".to_string())
        );
    }

    fn test_image(url: &str, urlbase: &str, startdate: &str, hsh: &str) -> BingImage {
        BingImage {
            url: url.to_string(),
            urlbase: Some(urlbase.to_string()),
            startdate: Some(startdate.to_string()),
            title: Some("Test Image".to_string()),
            copyright: Some("Test Copyright".to_string()),
            copyrightlink: None,
            hsh: Some(hsh.to_string()),
        }
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        env::temp_dir().join(format!("spotlit-{name}-{}-{unique}", std::process::id()))
    }
}
