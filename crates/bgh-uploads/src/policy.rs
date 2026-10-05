//! What may be uploaded, and how it is served: GitHub's allowed attachment
//! extensions and size limits (images 10 MB, videos 100 MB, other files
//! 25 MB). The content type is derived from the extension, never taken
//! from the client.

/// How an attachment is linked and served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Rendered inline (`![name](url)`), served inline.
    Image,
    /// SVG: rendered inline as an image, but served as a download (it can
    /// carry script).
    Svg,
    /// Bare URL in markdown (rendered as `<video>`), served inline, Range.
    Video,
    /// `[name](url)`, served as a download.
    File,
}

impl Kind {
    pub fn max_size(self) -> u64 {
        const MB: u64 = 1024 * 1024;
        match self {
            Kind::Image | Kind::Svg => 10 * MB,
            Kind::Video => 100 * MB,
            Kind::File => 25 * MB,
        }
    }

    /// Served with `Content-Disposition: inline`.
    pub fn inline(self) -> bool {
        matches!(self, Kind::Image | Kind::Video)
    }

    /// Linked under `/user-attachments/assets/{uuid}` (media) rather than
    /// `/user-attachments/files/{id}/{name}`.
    pub fn is_media(self) -> bool {
        !matches!(self, Kind::File)
    }
}

/// `(kind, content type)` for an allowed file name, by extension.
pub fn classify(name: &str) -> Option<(Kind, &'static str)> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    const TEXT: &str = "text/plain; charset=utf-8";
    Some(match ext.as_str() {
        "png" => (Kind::Image, "image/png"),
        "gif" => (Kind::Image, "image/gif"),
        "jpg" | "jpeg" => (Kind::Image, "image/jpeg"),
        "svg" => (Kind::Svg, "image/svg+xml"),
        "mp4" => (Kind::Video, "video/mp4"),
        "mov" => (Kind::Video, "video/quicktime"),
        "webm" => (Kind::Video, "video/webm"),
        "pdf" => (Kind::File, "application/pdf"),
        "zip" => (Kind::File, "application/zip"),
        "gz" | "tgz" => (Kind::File, "application/gzip"),
        "docx" => (
            Kind::File,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        "pptx" => (
            Kind::File,
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        "xlsx" => (
            Kind::File,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        "odt" | "fodt" => (Kind::File, "application/vnd.oasis.opendocument.text"),
        "ods" | "fods" => (Kind::File, "application/vnd.oasis.opendocument.spreadsheet"),
        "odp" | "fodp" => (
            Kind::File,
            "application/vnd.oasis.opendocument.presentation",
        ),
        "odg" | "fodg" => (Kind::File, "application/vnd.oasis.opendocument.graphics"),
        "odf" => (Kind::File, "application/vnd.oasis.opendocument.formula"),
        "rtf" => (Kind::File, "application/rtf"),
        "json" | "jsonc" | "cpuprofile" => (Kind::File, "application/json"),
        "csv" => (Kind::File, "text/csv; charset=utf-8"),
        "tsv" => (Kind::File, "text/tab-separated-values; charset=utf-8"),
        "html" | "htm" => (Kind::File, "text/html; charset=utf-8"),
        "eml" | "msg" => (Kind::File, "message/rfc822"),
        "dmp" => (Kind::File, "application/octet-stream"),
        "log" | "txt" | "md" | "patch" | "diff" | "c" | "cs" | "cpp" | "css" | "h" | "java"
        | "js" | "jsx" | "ts" | "tsx" | "py" | "rb" | "sh" | "sql" | "xml" | "yaml" | "yml"
        | "go" | "rs" | "toml" => (Kind::File, TEXT),
        _ => return None,
    })
}

/// Raster images must really be what their extension says (the browser
/// would show a broken image otherwise).
pub fn content_matches(content_type: &str, head: &[u8]) -> bool {
    match content_type {
        "image/png" => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a"),
        "image/jpeg" => head.starts_with(b"\xff\xd8\xff"),
        _ => true,
    }
}

/// Display name: the last path component, without control characters,
/// at most 255 bytes. Empty when nothing usable remains.
pub fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    while out.len() > 255 {
        out.remove(0);
    }
    if out.trim_matches('.').is_empty() {
        return String::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(classify("a.PNG").unwrap().0, Kind::Image);
        assert_eq!(classify("x.svg").unwrap().0, Kind::Svg);
        assert_eq!(
            classify("clip.mov").unwrap(),
            (Kind::Video, "video/quicktime")
        );
        assert_eq!(classify("build.log").unwrap().0, Kind::File);
        assert!(classify("evil.exe").is_none());
        assert!(classify("noext").is_none());
        assert!(!Kind::Svg.inline());
        assert!(Kind::Svg.is_media());
        assert!(!classify("page.html").unwrap().0.inline());
    }

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("C:\\Users\\me\\shot 1.png"), "shot 1.png");
        assert_eq!(clean_name("../../etc/passwd.txt"), "passwd.txt");
        assert_eq!(clean_name("a\u{0}b.png"), "ab.png");
        assert_eq!(clean_name(".."), "");
    }

    #[test]
    fn sniffs_images() {
        assert!(content_matches("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(!content_matches("image/png", b"<html>"));
        assert!(content_matches("text/plain; charset=utf-8", b"anything"));
    }
}
