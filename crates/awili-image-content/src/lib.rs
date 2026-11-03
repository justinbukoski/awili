//! Pure conversion of Awili's persisted inline image attachments to wire parts.
//! No URLs are fetched and no paths are opened. Original stored text is unchanged.
use base64::Engine;
use serde_json::{json, Value};

pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMAGES: usize = 8;
/// Conservative admission reserve, not measured usage or billing.
pub const IMAGE_TOKEN_RESERVE: u64 = 16384;

#[derive(Debug, Default)]
pub struct Content {
    pub text: String,
    pub images: Vec<(String, Vec<u8>)>,
}

/// Locate code once, before scanning attachments, so many malformed URLs cannot
/// cause a quadratic UI-thread scan. This only preserves source; no code runs.
fn code_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    let mut fence: Option<&str> = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(open) = fence {
            ranges.push(offset..offset + line.len());
            if marker == Some(open) {
                fence = None;
            }
        } else if let Some(marker) = marker {
            fence = Some(marker);
            ranges.push(offset..offset + line.len());
        } else {
            let mut rest = line;
            let mut consumed = 0;
            while let Some(start) = rest.find('`') {
                let count = rest[start..].bytes().take_while(|b| *b == b'`').count();
                let after = start + count;
                let marker = &rest[start..after];
                let Some(end) = rest[after..].find(marker) else {
                    break;
                };
                let end = after + end + count;
                ranges.push(offset + consumed + start..offset + consumed + end);
                consumed += end;
                rest = &rest[end..];
            }
        }
        offset += line.len();
    }
    ranges
}

/// Extract only embedded data URIs. Unsupported, malformed and excessive data
/// remains an explicit notice, never a megabyte of base64 in a text channel.
pub fn split(source: &str) -> Content {
    let mut out = Content::default();
    let mut rest = source;
    let mut total = 0;
    let code = code_ranges(source);
    while let Some(start) = rest.find("data:image/") {
        // Only the persisted Markdown attachment shape is image input. Literal
        // data URLs in code/CSS/documentation remain ordinary source text.
        let prefix = &rest[..start];
        let attachment = prefix.ends_with("](")
            && prefix
                .rfind("![")
                .is_some_and(|i| !prefix[i..].contains('\n') && prefix.len() - i < 4096);
        let absolute = source.len() - rest.len() + start;
        let code_index = code.partition_point(|range| range.end <= absolute);
        let in_code = code
            .get(code_index)
            .is_some_and(|range| range.contains(&absolute));
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c == ')' || c == '"' || c == '\\' || c.is_whitespace())
            .unwrap_or(tail.len());
        let before_line = prefix.rsplit('\n').next().unwrap_or("");
        let after_uri = &tail[end..];
        let standalone = before_line.trim_start().starts_with("![")
            && after_uri.starts_with(')')
            && after_uri[1..]
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .is_empty();
        if !attachment || (in_code && !standalone) {
            out.text.push_str(&rest[..start]);
            // A large encoded image is never prose sent to a model, including
            // malformed old history and literal examples. Original storage and
            // Copy are untouched; small code examples keep their exact source.
            if end > 4096 && tail[..end].contains(";base64,") {
                out.text.push_str("image-data-omitted-from-text");
            } else {
                out.text.push_str(&tail[..end]);
            }
            rest = &tail[end..];
            continue;
        }
        out.text.push_str(&rest[..start]);
        rest = tail;
        let uri = &rest[..end];
        let image = uri.split_once(";base64,").and_then(|(mime, b64)| {
            let mime = mime.strip_prefix("data:")?;
            if !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) || b64.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
                || out.images.len() >= MAX_IMAGES
            {
                return None;
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            if bytes.len() > MAX_IMAGE_BYTES || total + bytes.len() > 16 * 1024 * 1024 {
                return None;
            }
            let format = match mime {
                "image/png" => image::ImageFormat::Png,
                "image/jpeg" => image::ImageFormat::Jpeg,
                "image/gif" => image::ImageFormat::Gif,
                "image/webp" => image::ImageFormat::WebP,
                _ => return None,
            };
            if validated(&bytes, format) {
                return Some((mime.to_string(), bytes));
            }
            let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().ok()?;
            cache_validated(&bytes, format);
            Some((mime.to_string(), bytes))
        });
        if let Some(image) = image {
            total += image.1.len();
            out.images.push(image);
            out.text.push_str("embedded-image");
        } else {
            out.text.push_str("image-unavailable-or-over-limit");
        }
        rest = &rest[end..];
    }
    out.text.push_str(rest);
    out
}

type ValidatedCache = std::collections::VecDeque<(u64, image::ImageFormat, Vec<u8>)>;
static VALIDATED: std::sync::OnceLock<std::sync::Mutex<ValidatedCache>> =
    std::sync::OnceLock::new();
fn image_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}
fn validated(bytes: &[u8], format: image::ImageFormat) -> bool {
    let hash = image_hash(bytes);
    VALIDATED
        .get_or_init(Default::default)
        .lock()
        .ok()
        .is_some_and(|cache| {
            cache
                .iter()
                .any(|(h, f, b)| *h == hash && *f == format && b == bytes)
        })
}
fn cache_validated(bytes: &[u8], format: image::ImageFormat) {
    if let Ok(mut cache) = VALIDATED.get_or_init(Default::default).lock() {
        let hash = image_hash(bytes);
        if cache
            .iter()
            .any(|(h, f, b)| *h == hash && *f == format && b == bytes)
        {
            return;
        }
        while cache.len() >= MAX_IMAGES
            || cache.iter().map(|(_, _, b)| b.len()).sum::<usize>() + bytes.len() > 16 * 1024 * 1024
        {
            cache.pop_front();
        }
        cache.push_back((hash, format, bytes.to_vec()));
    }
}

pub fn uri(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

pub fn openai(source: &str) -> Value {
    let c = split(source);
    if c.images.is_empty() {
        return json!(c.text);
    }
    let mut parts = vec![json!({"type":"text","text":c.text})];
    parts.extend(
        c.images
            .iter()
            .map(|(m, b)| json!({"type":"image_url","image_url":{"url":uri(m,b)}})),
    );
    json!(parts)
}

pub fn codex(source: &str) -> Value {
    let c = split(source);
    let mut parts = vec![json!({"type":"text","text":c.text})];
    parts.extend(
        c.images
            .iter()
            .map(|(m, b)| json!({"type":"image","url":uri(m,b)})),
    );
    json!(parts)
}

pub fn anthropic(source: &str) -> Value {
    let c = split(source);
    if c.images.is_empty() {
        return json!(c.text);
    }
    let mut parts = vec![json!({"type":"text","text":c.text})];
    parts.extend(c.images.iter().map(|(m,b)|json!({"type":"image","source":{"type":"base64","media_type":m,"data":base64::engine::general_purpose::STANDARD.encode(b)}})));
    json!(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> String {
        format!(
            "![screen.png]({})\nWhat is here?",
            uri("image/png", b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\x0d\x49\x44\x41\x54\x78\x9c\x63\xf8\xcf\xc0\xf0\x1f\x00\x05\x00\x01\xff\x89\x99\x3d\x1d\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82")
        )
    }
    #[test]
    fn image_bytes_are_wire_parts_and_never_prompt_text() {
        let s = sample();
        let c = split(&s);
        assert_eq!(c.images.len(), 1);
        assert!(!c.text.contains("base64"));
        let o = openai(&s);
        assert_eq!(o[1]["type"], "image_url");
        assert_eq!(o[0]["text"], c.text);
        let n = codex(&s);
        assert_eq!(n[1]["type"], "image");
        let a = anthropic(&s);
        assert_eq!(a[1]["source"]["media_type"], "image/png");
        assert_eq!(openai("hello"), json!("hello"));
    }
    #[test]
    fn invalid_unsupported_and_excess_images_are_explicit_and_bounded() {
        for s in [
            "![broken](data:image/png;base64,broken!)",
            "![svg](data:image/svg+xml;base64,PHN2Zz4=)",
        ] {
            let c = split(s);
            assert!(c.images.is_empty());
            assert!(c.text.contains("unavailable"));
            assert!(!c.text.contains("base64"));
        }
        let c = split(&sample().repeat(12));
        assert_eq!(c.images.len(), MAX_IMAGES);
        assert!(!c.text.contains("base64"));
    }
    #[test]
    fn json_history_and_multiple_images_do_not_leak_encoded_text() {
        let s = json!({"history":[{"content":sample()}]}).to_string();
        let c = split(&s);
        assert_eq!(c.images.len(), 1);
        serde_json::from_str::<Value>(&c.text).unwrap();
        assert_eq!(split(&format!("{} {}", sample(), sample())).images.len(), 2);
    }
    #[test]
    fn literal_urls_in_code_are_unchanged_and_truncated_files_are_rejected() {
        for source in [
            "const image = \"data:image/png;base64,iVBORw0KGgo=\";",
            "`![example](data:image/png;base64,iVBORw0KGgo=)`",
        ] {
            assert_eq!(split(source).text, source);
            assert!(split(source).images.is_empty());
        }
        let broken = split("![truncated](data:image/png;base64,iVBORw0KGgo=)");
        assert!(broken.images.is_empty());
        assert!(broken.text.contains("unavailable"));
    }
    #[test]
    fn real_attachment_survives_truncated_text_fences_and_large_literal_data_never_becomes_prose() {
        let image = sample();
        for prefix in [
            "```notes.md\n```rust\nunfinished\n```\n\n",
            "```unfinished\n",
        ] {
            let c = split(&format!("{prefix}{image}"));
            assert_eq!(c.images.len(), 1);
            assert!(!c.text.contains("base64"));
        }
        let literal = format!("let uri = \"data:image/png;base64,{}\"", "A".repeat(10000));
        assert!(!split(&literal).text.contains(&"A".repeat(1000)));
        assert!(split("ends with data:image/png;base64,small")
            .images
            .is_empty());
    }
}
