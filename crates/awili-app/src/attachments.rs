//! File/attachment helpers: the `~/Downloads` target path, an extension->mime
//! guess, the attachment-into-message-text composer, and the tray chip label.
//! Extracted from `main.rs` as part of the full-review decomposition. All pure;
//! the export/attachment wiring tests stay in `main.rs`.

use awili_persist::{format_attachment_for_message, Attachment};

/// `~/Downloads/<filename>` (slice 17 §2): the platform downloads dir when
/// the OS reports one, else `<home>/Downloads`.
pub(crate) fn downloads_path(filename: &str) -> Option<std::path::PathBuf> {
    let dir = dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))?;
    Some(dir.join(filename))
}

/// Guess a mime type from a file's extension (slice 19 §4) — only as fine-
/// grained as the attachment formatter's three families need (text-like,
/// image, other). Unknown extensions land on `application/octet-stream`,
/// which formats as the unsupported-attachment notice.
pub(crate) fn mime_for_path(path: &std::path::Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        Some(
            "txt" | "log" | "rs" | "py" | "js" | "ts" | "tsx" | "css" | "sh" | "yaml" | "yml"
            | "ini" | "cfg" | "conf",
        ) => "text/plain",
        Some("json") => "application/json",
        Some("toml") => "application/toml",
        Some("xml") => "application/xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Prepend each staged attachment's formatted content to the typed message
/// (slice 19 §7), blocks separated by blank lines. The result is what gets
/// pushed, persisted, and sent — attachments are part of the message text, so
/// they survive into export and search.
pub(crate) fn compose_message_with_attachments(text: &str, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    let mut parts: Vec<String> = attachments
        .iter()
        .map(format_attachment_for_message)
        .collect();
    parts.push(text.to_string());
    parts.join("\n\n")
}

/// The tray chip's text (slice 19 §6): `<filename> (<size>KB)`, size
/// rounded UP so a non-empty file never reads `0KB`. The paperclip is drawn
/// by the view as a wireframe icon, not baked into the string.
pub(crate) fn attachment_chip_label(att: &Attachment) -> String {
    format!("{} ({}KB)", att.filename, (att.size_bytes + 1023) / 1024)
}
