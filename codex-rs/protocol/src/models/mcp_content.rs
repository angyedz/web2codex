//! Converts individual MCP content blocks into model-visible tool output.

use super::DEFAULT_IMAGE_DETAIL;
use super::FunctionCallOutputContentItem;
use super::ImageDetail;
use super::ImageReference;
use crate::mcp::Resource;
use crate::mcp::ResourceContent;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum McpContent<'a> {
    Text {
        text: String,
    },
    Image(#[serde(borrow)] McpMedia<'a>),
    Audio(#[serde(borrow)] McpMedia<'a>),
    ResourceLink {
        #[serde(flatten)]
        link: Resource,
    },
    Resource {
        #[serde(rename = "resource")]
        resource: ResourceContent,
    },
}

#[derive(Deserialize)]
struct McpMedia<'a> {
    data: &'a str,
    #[serde(rename = "mimeType", alias = "mime_type")]
    mime_type: Option<&'a str>,
}

impl McpMedia<'_> {
    fn data_url(&self) -> String {
        data_url(
            self.mime_type.unwrap_or("application/octet-stream"),
            self.data,
        )
    }
}

impl FunctionCallOutputContentItem {
    /// Projects one MCP ContentBlock using its wire discriminator and Codex metadata.
    pub fn from_mcp_content_block(block: &Value) -> serde_json::Result<Self> {
        let meta = block.get("_meta");
        Ok(match McpContent::deserialize(block)? {
            McpContent::Text { text } => {
                if meta
                    .and_then(|meta| meta.get("codex/encryptedContent"))
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    Self::EncryptedContent {
                        encrypted_content: text,
                    }
                } else {
                    Self::InputText { text }
                }
            }
            McpContent::Image(media) => Self::InputImage {
                image: ImageReference::Inline {
                    image_url: media.data_url(),
                },
                detail: Some(
                    meta.and_then(|meta| meta.get("codex/imageDetail"))
                        .filter(|detail| detail.is_string())
                        .and_then(|detail| ImageDetail::deserialize(detail).ok())
                        .unwrap_or(DEFAULT_IMAGE_DETAIL),
                ),
            },
            McpContent::Audio(media) => Self::inline_audio(media.data_url()),
            McpContent::ResourceLink { link } => Self::InputText {
                text: resource_link_text(&link),
            },
            McpContent::Resource { resource } => resource_content_item(resource),
        })
    }
}

fn resource_link_text(link: &Resource) -> String {
    let mut lines = vec![
        format!(
            "MCP resource: {}",
            redact_data_uris(link.title.as_deref().unwrap_or(&link.name))
        ),
        format!("URI: {}", redact_data_uris(&link.uri)),
    ];
    if let Some(description) = link.description.as_deref().filter(|text| !text.is_empty()) {
        lines.push(format!("Description: {}", redact_data_uris(description)));
    }
    if let Some(mime_type) = link.mime_type.as_deref().filter(|text| !text.is_empty()) {
        lines.push(format!("MIME type: {mime_type}"));
    }
    if let Some(size) = link.size {
        lines.push(format!("Size: {size} bytes"));
    }
    lines.join("\n")
}

fn resource_content_item(resource: ResourceContent) -> FunctionCallOutputContentItem {
    match resource {
        ResourceContent::Text {
            uri,
            mime_type,
            text,
            ..
        } => FunctionCallOutputContentItem::InputText {
            text: format!(
                "MCP resource content ({}) at {}:\n{}",
                mime_type.as_deref().unwrap_or("text/plain"),
                redact_data_uris(&uri),
                text,
            ),
        },
        ResourceContent::Blob {
            uri,
            mime_type,
            blob,
            ..
        } => {
            let mime_type = mime_type
                .as_deref()
                .or_else(|| data_uri_mime_type(&uri))
                .unwrap_or("application/octet-stream");
            if mime_type
                .get(.."image/".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
            {
                FunctionCallOutputContentItem::InputImage {
                    image: ImageReference::Inline {
                        image_url: data_url(mime_type, &blob),
                    },
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                }
            } else if mime_type
                .get(.."audio/".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("audio/"))
            {
                FunctionCallOutputContentItem::inline_audio(data_url(mime_type, &blob))
            } else {
                FunctionCallOutputContentItem::InputFile {
                    file_url: data_url(mime_type, &blob),
                    filename: safe_mcp_filename(mime_type),
                }
            }
        }
    }
}

/// MCP resource URIs are untrusted and may contain path components or control
/// characters. Use a fixed base name and derive only a validated extension
/// from the MIME type so the model receives a safe, useful filename.
fn safe_mcp_filename(mime_type: &str) -> String {
    format!("mcp-resource.{}", safe_mime_extension(mime_type))
}

fn safe_mime_extension(mime_type: &str) -> String {
    let mime_type = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    let known_extension = match mime_type.as_str() {
        "application/octet-stream" => Some("bin"),
        "application/pdf" => Some("pdf"),
        "application/json" | "application/ld+json" => Some("json"),
        "application/x-ndjson" => Some("ndjson"),
        "application/xml" | "text/xml" => Some("xml"),
        "application/zip" | "application/x-zip-compressed" => Some("zip"),
        "application/gzip" | "application/x-gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/rtf" => Some("rtf"),
        "application/msword" => Some("doc"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.ms-excel" => Some("xls"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => Some("xlsx"),
        "application/vnd.ms-powerpoint" => Some("ppt"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        "text/plain" => Some("txt"),
        "text/csv" => Some("csv"),
        "text/markdown" => Some("md"),
        "text/html" => Some("html"),
        _ => None,
    };
    if let Some(extension) = known_extension {
        return extension.to_string();
    }

    let Some((_, subtype)) = mime_type.split_once('/') else {
        return "bin".to_string();
    };
    // Structured suffixes such as `+json` and vendor-tree names provide a
    // conventional extension when there is no explicit mapping above.
    let subtype = subtype
        .rsplit_once('+')
        .map_or(subtype, |(_, suffix)| suffix)
        .strip_prefix("vnd.")
        .unwrap_or_else(|| {
            subtype
                .rsplit_once('+')
                .map_or(subtype, |(_, suffix)| suffix)
        })
        .rsplit('.')
        .next()
        .unwrap_or_default();
    let subtype = subtype.strip_prefix("x-").unwrap_or(subtype);
    if !subtype.is_empty()
        && subtype.len() <= 16
        && subtype
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        subtype.to_string()
    } else {
        "bin".to_string()
    }
}

fn data_url(mime_type: &str, data: &str) -> String {
    if data
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        data.to_owned()
    } else {
        format!("data:{mime_type};base64,{data}")
    }
}

fn data_uri_mime_type(uri: &str) -> Option<&str> {
    let rest = uri
        .get(.."data:".len())
        .filter(|prefix| prefix.eq_ignore_ascii_case("data:"))
        .and_then(|_| uri.get("data:".len()..))?;
    let (mime_type, _) = rest.split_once(',')?;
    let mime_type = mime_type.split(';').next().unwrap_or_default();
    (!mime_type.is_empty()).then_some(mime_type)
}

/// Replaces inline data URIs before writing text into the model-visible tool output.
fn redact_data_uris(text: &str) -> String {
    if !contains_data_uri(text) {
        return text.to_owned();
    }

    let mut redacted = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(offset) = text.as_bytes()[cursor..]
        .windows("data:".len())
        .position(|candidate| candidate.eq_ignore_ascii_case(b"data:"))
    {
        let start = cursor + offset;
        redacted.push_str(&text[cursor..start]);
        let end = text[start..]
            .find([' ', '\t', '\r', '\n', '"', '\'', '<', '>', ')', ']', '}'])
            .map_or(text.len(), |offset| start + offset);
        let uri = &text[start..end];
        let encoded_len = uri
            .split_once(',')
            .map(|(_, payload)| payload.len())
            .unwrap_or(0);
        if encoded_len == 0 {
            redacted.push_str("data:[inline payload omitted]");
        } else {
            redacted.push_str(&format!(
                "data:[inline payload omitted; {encoded_len} characters]"
            ));
        }
        cursor = end;
    }
    redacted.push_str(&text[cursor..]);
    redacted
}

fn contains_data_uri(text: &str) -> bool {
    text.as_bytes()
        .windows("data:".len())
        .any(|candidate| candidate.eq_ignore_ascii_case(b"data:"))
}

fn safe_unknown_block_text(block: &Value) -> String {
    let kind = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![format!("Unsupported MCP content block ({kind}).")];
    for (key, label) in [
        ("name", "Name"),
        ("title", "Title"),
        ("uri", "URI"),
        ("description", "Description"),
        ("mimeType", "MIME type"),
        ("mime_type", "MIME type"),
    ] {
        if let Some(value) = block
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("{label}: {}", redact_data_uris(value)));
        }
    }
    if matches!(kind, "image" | "audio") {
        lines.push("Media payload unavailable or invalid.".to_string());
    } else if block.get("data").is_some() || block.get("blob").is_some() {
        lines.push("Binary payload omitted.".to_string());
    }
    lines.join("\n")
}

pub(super) fn convert_mcp_content_to_items(
    contents: &[Value],
) -> Vec<FunctionCallOutputContentItem> {
    contents
        .iter()
        .map(|block| {
            FunctionCallOutputContentItem::from_mcp_content_block(block).unwrap_or_else(|_| {
                FunctionCallOutputContentItem::InputText {
                    text: safe_unknown_block_text(block),
                }
            })
        })
        .collect()
}
