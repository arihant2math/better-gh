//! Content types by file name.

/// Content type for a file name (by extension); `application/octet-stream`
/// when unknown.
pub fn for_path(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

pub fn is_image(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/tiff"
}

/// Content type for raw responses: text is always served as `text/plain`
/// (never HTML/JS on our origin); binary files by extension.
pub fn for_raw(name: &str, binary: bool) -> &'static str {
    let by_name = for_path(name);
    if by_name != "application/octet-stream" && (binary || by_name == "image/svg+xml") {
        by_name
    } else if binary {
        "application/octet-stream"
    } else {
        "text/plain; charset=utf-8"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types() {
        assert_eq!(for_raw("a.PNG", true), "image/png");
        assert_eq!(for_raw("index.html", false), "text/plain; charset=utf-8");
        assert_eq!(for_raw("logo.svg", false), "image/svg+xml");
        assert_eq!(for_raw("blob.bin", true), "application/octet-stream");
        assert!(is_image(for_path("x.gif")));
    }
}
