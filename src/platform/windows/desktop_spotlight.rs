use std::path::PathBuf;

use crate::core::{DesktopSpotlightCreative, Result, SpotlightMetadata, SpotlitError};
use serde::Deserialize;

use super::registry;

const SPOTLIGHT_NAMESPACE_KEY: &str =
    "Software\\Classes\\CLSID\\{2cc5ca98-6485-489a-920e-b3e88a6ccce3}";
const SPOTLIGHT_CLICK_KEY: &str =
    "Software\\Classes\\CLSID\\{2cc5ca98-6485-489a-920e-b3e88a6ccce3}\\shell\\SpotlightClick";
const DESKTOP_SPOTLIGHT_CREATIVES_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\DesktopSpotlight\\Creatives";

pub fn current_desktop_spotlight_metadata() -> Result<Option<SpotlightMetadata>> {
    let edge_uri = registry::read_hkcu_string(SPOTLIGHT_CLICK_KEY, "EdgeUri")?;
    let info_tip = registry::read_hkcu_string(SPOTLIGHT_NAMESPACE_KEY, "InfoTip")?;
    let content_id = registry::read_hkcu_string(SPOTLIGHT_CLICK_KEY, "contentId")?;

    Ok(metadata_from_registry_values(
        edge_uri.as_deref(),
        info_tip.as_deref(),
        content_id.as_deref(),
    ))
}

pub fn desktop_spotlight_creatives() -> Result<Vec<DesktopSpotlightCreative>> {
    let Some(creatives_json) =
        registry::read_hkcu_string(DESKTOP_SPOTLIGHT_CREATIVES_KEY, "Creatives")?
    else {
        return Ok(Vec::new());
    };
    let image_index = registry::read_hkcu_dword(DESKTOP_SPOTLIGHT_CREATIVES_KEY, "ImageIndex")?;

    parse_desktop_spotlight_creatives(&creatives_json, image_index)
}

fn metadata_from_registry_values(
    edge_uri: Option<&str>,
    info_tip: Option<&str>,
    content_id: Option<&str>,
) -> Option<SpotlightMetadata> {
    let info_url = edge_uri.and_then(normalize_spotlight_url);
    let query = info_url
        .as_deref()
        .map(parse_spotlight_query)
        .unwrap_or_default();
    let parsed_tip = info_tip.map(parse_info_tip).unwrap_or_default();

    let title = query.title.or_else(|| parsed_tip.caption.clone());
    let caption = parsed_tip.caption.filter(|caption| {
        title
            .as_deref()
            .is_none_or(|title| !caption.eq_ignore_ascii_case(title))
    });

    let metadata = SpotlightMetadata {
        spotlight_id: query.spotlight_id,
        title,
        caption,
        copyright: parsed_tip.copyright,
        info_url,
        content_id: content_id.map(ToOwned::to_owned),
    }
    .normalized();

    (!metadata.is_empty()).then_some(metadata)
}

fn parse_desktop_spotlight_creatives(
    creatives_json: &str,
    image_index: Option<u32>,
) -> Result<Vec<DesktopSpotlightCreative>> {
    let records: Vec<DesktopSpotlightCreativeRecord> = serde_json::from_str(creatives_json)
        .map_err(|source| {
            SpotlitError::platform(format!(
                "parse desktop Spotlight creatives registry JSON: {source}"
            ))
        })?;

    Ok(records
        .into_iter()
        .enumerate()
        .filter_map(|(index, record)| {
            desktop_spotlight_creative_from_record(
                record,
                image_index.is_some_and(|image_index| image_index as usize == index),
            )
        })
        .collect())
}

fn desktop_spotlight_creative_from_record(
    record: DesktopSpotlightCreativeRecord,
    is_current: bool,
) -> Option<DesktopSpotlightCreative> {
    let ad = record.ad?;
    let landscape_path = ad.landscape_image.as_ref()?.asset.clone()?;
    if landscape_path.as_os_str().is_empty() {
        return None;
    }

    Some(DesktopSpotlightCreative {
        landscape_path,
        portrait_path: ad
            .portrait_image
            .as_ref()
            .and_then(|image| image.asset.clone()),
        metadata: metadata_from_creative_ad(&ad),
        is_current,
    })
}

fn metadata_from_creative_ad(ad: &DesktopSpotlightAd) -> SpotlightMetadata {
    let info_url = ad.cta_uri.as_deref().and_then(normalize_spotlight_url);
    let query = info_url
        .as_deref()
        .map(parse_spotlight_query)
        .unwrap_or_default();
    let parsed_tip = ad
        .icon_hover_text
        .as_deref()
        .map(parse_info_tip)
        .unwrap_or_default();

    let title = query
        .title
        .or_else(|| parsed_tip.caption.clone())
        .or_else(|| ad.title.clone().and_then(non_empty));
    let caption = parsed_tip.caption.filter(|caption| {
        title
            .as_deref()
            .is_none_or(|title| !caption.eq_ignore_ascii_case(title))
    });

    SpotlightMetadata {
        spotlight_id: query.spotlight_id,
        title,
        caption,
        copyright: ad
            .copyright
            .clone()
            .and_then(non_empty)
            .or(parsed_tip.copyright),
        info_url,
        content_id: ad.entity_id.clone().and_then(non_empty),
    }
    .normalized()
}

#[derive(Debug, Deserialize)]
struct DesktopSpotlightCreativeRecord {
    ad: Option<DesktopSpotlightAd>,
}

#[derive(Debug, Deserialize)]
struct DesktopSpotlightAd {
    #[serde(rename = "landscapeImage")]
    landscape_image: Option<DesktopSpotlightImage>,
    #[serde(rename = "portraitImage")]
    portrait_image: Option<DesktopSpotlightImage>,
    #[serde(rename = "iconHoverText")]
    icon_hover_text: Option<String>,
    title: Option<String>,
    copyright: Option<String>,
    #[serde(rename = "ctaUri")]
    cta_uri: Option<String>,
    #[serde(rename = "entityId")]
    entity_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DesktopSpotlightImage {
    asset: Option<PathBuf>,
}

fn normalize_spotlight_url(edge_uri: &str) -> Option<String> {
    let uri = edge_uri.trim();
    let url = strip_prefix_ignore_ascii_case(uri, "microsoft-edge:").unwrap_or(uri);

    if is_bing_spotlight_url(url) {
        Some(url.to_string())
    } else {
        None
    }
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then_some(&value[prefix.len()..])
}

fn is_bing_spotlight_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://www.bing.com/spotlight?")
        || lower.starts_with("https://bing.com/spotlight?")
}

#[derive(Debug, Default)]
struct SpotlightQuery {
    spotlight_id: Option<String>,
    title: Option<String>,
}

fn parse_spotlight_query(url: &str) -> SpotlightQuery {
    let Some(query) = url.split_once('?').map(|(_, query)| query) else {
        return SpotlightQuery::default();
    };
    let query = query.split_once('#').map_or(query, |(query, _)| query);

    let mut parsed = SpotlightQuery::default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key.to_ascii_lowercase().as_str() {
            "spotlightid" => parsed.spotlight_id = non_empty(percent_decode_query_value(value)),
            "q" => parsed.title = non_empty(percent_decode_query_value(value)),
            _ => {}
        }
    }

    parsed
}

#[derive(Debug, Default)]
struct ParsedInfoTip {
    caption: Option<String>,
    copyright: Option<String>,
}

fn parse_info_tip(info_tip: &str) -> ParsedInfoTip {
    let mut parsed = ParsedInfoTip::default();

    for line in info_tip
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line.to_ascii_lowercase().contains("right-click") {
            continue;
        }

        if line.starts_with('\u{00a9}') {
            parsed.copyright = Some(line.to_string());
        } else if parsed.caption.is_none() {
            parsed.caption = Some(line.to_string());
        }
    }

    parsed
}

fn percent_decode_query_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1]);
                let low = hex_value(bytes[index + 2]);
                if let (Some(high), Some(low)) = (high, low) {
                    output.push((high << 4) | low);
                    index += 3;
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_spotlight_registry_values() {
        let metadata = metadata_from_registry_values(
            Some(
                "microsoft-edge:https://www.bing.com/spotlight?spotlightid=DS_ArchwaySpitzkoppe&q=Spitzkoppe%2C+Namibia&FORM=MC13ER",
            ),
            Some(
                "'Eye of Spitzkoppe,' Namibia\n\
                 \u{00a9} Simon Phelps Photography / Moment / Getty Images\n\
                 Right-click to learn more",
            ),
            Some("128000000004965589"),
        )
        .expect("metadata was parsed");

        assert_eq!(
            metadata.spotlight_id.as_deref(),
            Some("DS_ArchwaySpitzkoppe")
        );
        assert_eq!(metadata.title.as_deref(), Some("Spitzkoppe, Namibia"));
        assert_eq!(
            metadata.caption.as_deref(),
            Some("'Eye of Spitzkoppe,' Namibia")
        );
        assert_eq!(
            metadata.copyright.as_deref(),
            Some("\u{00a9} Simon Phelps Photography / Moment / Getty Images")
        );
        assert_eq!(
            metadata.info_url.as_deref(),
            Some(
                "https://www.bing.com/spotlight?spotlightid=DS_ArchwaySpitzkoppe&q=Spitzkoppe%2C+Namibia&FORM=MC13ER"
            )
        );
        assert_eq!(metadata.content_id.as_deref(), Some("128000000004965589"));
    }

    #[test]
    fn ignores_non_spotlight_edge_urls() {
        let metadata = metadata_from_registry_values(
            Some("microsoft-edge:https://www.bing.com/search?q=wallpaper"),
            None,
            None,
        );

        assert!(metadata.is_none());
    }

    #[test]
    fn decodes_query_values_as_utf8() {
        let query = parse_spotlight_query(
            "https://www.bing.com/spotlight?spotlightid=DS_Test&q=S%C3%A3o+Paulo",
        );

        assert_eq!(query.spotlight_id.as_deref(), Some("DS_Test"));
        assert_eq!(query.title.as_deref(), Some("S\u{00e3}o Paulo"));
    }

    #[test]
    fn parses_desktop_spotlight_creatives_json() {
        let creatives = parse_desktop_spotlight_creatives(
            r#"[
                {
                    "ad": {
                        "landscapeImage": {
                            "asset": "C:\\Users\\root\\AppData\\Local\\Packages\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\LocalCache\\Microsoft\\IrisService\\a\\134254796758416639.jpg"
                        },
                        "portraitImage": {
                            "asset": "C:\\Users\\root\\AppData\\Local\\Packages\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\LocalCache\\Microsoft\\IrisService\\a\\134254796799418063.jpg"
                        },
                        "iconHoverText": "'Eye of Spitzkoppe,' Namibia\r\n© Simon Phelps Photography / Moment / Getty Images\r\nRight-click to learn more",
                        "title": "I spy…",
                        "copyright": "© Simon Phelps Photography / Moment / Getty Images",
                        "ctaUri": "microsoft-edge:https://www.bing.com/spotlight?spotlightid=DS_ArchwaySpitzkoppe&q=Spitzkoppe%2C+Namibia&FORM=MC13ER",
                        "entityId": "128000000004965589"
                    }
                },
                {
                    "ad": {
                        "landscapeImage": {
                            "asset": "C:\\Users\\root\\AppData\\Local\\Packages\\MicrosoftWindows.Client.CBS_cw5n1h2txyewy\\LocalCache\\Microsoft\\IrisService\\b\\134254796819459119.jpg"
                        },
                        "iconHoverText": "Porto Venere, Italy\r\n© Roberto Moiola / Sysaworld / Moment / Getty Images\r\nRight-click to learn more",
                        "title": "A poetic port",
                        "copyright": "© Roberto Moiola / Sysaworld / Moment / Getty Images",
                        "ctaUri": "microsoft-edge:https://www.bing.com/spotlight?spotlightid=DS_SanPietroPortovenere&q=Porto+Venere%2C+Italy&FORM=MC13ER",
                        "entityId": "128000000005548149"
                    }
                },
                {
                    "ad": {
                        "portraitImage": {
                            "asset": "C:\\portrait-only.jpg"
                        }
                    }
                }
            ]"#,
            Some(1),
        )
        .expect("creatives were parsed");

        assert_eq!(creatives.len(), 2);
        assert!(!creatives[0].is_current);
        assert_eq!(
            creatives[0].metadata.title.as_deref(),
            Some("Spitzkoppe, Namibia")
        );
        assert_eq!(
            creatives[0].metadata.caption.as_deref(),
            Some("'Eye of Spitzkoppe,' Namibia")
        );
        assert_eq!(
            creatives[0].metadata.spotlight_id.as_deref(),
            Some("DS_ArchwaySpitzkoppe")
        );
        assert_eq!(
            creatives[0].metadata.content_id.as_deref(),
            Some("128000000004965589")
        );

        assert!(creatives[1].is_current);
        assert_eq!(
            creatives[1].metadata.title.as_deref(),
            Some("Porto Venere, Italy")
        );
        assert_eq!(creatives[1].metadata.caption, None);
        assert_eq!(
            creatives[1].metadata.copyright.as_deref(),
            Some("© Roberto Moiola / Sysaworld / Moment / Getty Images")
        );
    }
}
