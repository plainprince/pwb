//! Downloads: saving a page as a `.pwb` bundle, its images, and any file the
//! browser does not render.
//!
//! Two flows use this module:
//!
//! * **Save page** (`Ctrl/Cmd+S`): writes the current page's bundle bytes as
//!   `<title>.pwb` plus a `<title>_files/` directory containing the page's
//!   images (inline `data:` images and on-disk assets), the same "page,
//!   complete" layout desktop browsers use.
//! * **Download a target**: a link or address that points at a file PlainWeb
//!   cannot render (`file.pwb/photo.png`, `/tmp/report.pdf`, a `data:` URL)
//!   is written to the downloads directory instead of failing to load.
//!
//! Files land in `$PWB_DOWNLOADS` when set, else `~/Downloads` when it exists,
//! else `<home>/.pwb/downloads`. Existing files are never overwritten: a
//! numeric suffix is added instead.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// Environment override for the downloads directory.
pub const DOWNLOAD_DIR_ENV: &str = "PWB_DOWNLOADS";

/// One ready-to-write download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What a page save produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageSave {
    /// The written `.pwb` bundle.
    pub bundle: PathBuf,
    /// Saved images beside it, in `<stem>_files/`.
    pub assets: Vec<PathBuf>,
}

/// Resolve the downloads directory, creating it.
pub fn downloads_dir() -> io::Result<PathBuf> {
    let dir = std::env::var_os(DOWNLOAD_DIR_ENV)
        .map(PathBuf::from)
        .or_else(|| {
            pwb_core::paths::home_dir().map(|home| {
                let downloads = home.join("Downloads");
                if downloads.is_dir() {
                    downloads
                } else {
                    home.join(".pwb").join("downloads")
                }
            })
        })
        .unwrap_or_else(|| PathBuf::from("pwb-downloads"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A filesystem-safe stem derived from a page title or URL. Never empty.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch.is_whitespace()
            || ch == '-'
            || ch == '_'
            || ch == '.'
            || ch == '/'
            || ch == '\\'
            || ch == ':')
            && !last_dash
        {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "page".to_string()
    } else {
        trimmed
    }
}

/// A non-colliding path in `dir` for `stem.ext`.
pub fn unique_path(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let mut candidate = if extension.is_empty() {
        dir.join(stem)
    } else {
        dir.join(format!("{stem}.{extension}"))
    };
    let mut counter = 1;
    while candidate.exists() {
        let name = if extension.is_empty() {
            format!("{stem}-{counter}")
        } else {
            format!("{stem}-{counter}.{extension}")
        };
        candidate = dir.join(name);
        counter += 1;
    }
    candidate
}

/// Write `bytes` into `dir` under `stem`/`extension`, never overwriting.
pub fn write_file(dir: &Path, stem: &str, extension: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, stem, extension);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// The bundle filename for a page: the title when it is meaningful, else the
/// URL's host, always with the `.pwb` extension.
pub fn page_name(url: &str, title: &str) -> String {
    let title = title.trim();
    let base = if title.is_empty() || title.eq_ignore_ascii_case(url) {
        crate::shell::shorten_url(url)
    } else {
        title.to_string()
    };
    format!("{}.pwb", slug(&base))
}

/// Decode a `data:` URL into `(mime, bytes)`. Handles base64 payloads, which is
/// how PlainWeb pages embed images, plus percent-encoded text.
pub fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let base64 = meta.ends_with(";base64");
    let mime = meta
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let bytes = if base64 {
        BASE64.decode(data.trim()).ok()?
    } else {
        percent_decode(data)
    };
    Some((mime, bytes))
}

/// Minimal percent-decoding for non-base64 data URLs.
fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Extension for a media type, when it is one we recognize.
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let mime = mime.split(';').next().unwrap_or("").trim();
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/json" => "json",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/css" => "css",
        "text/javascript" | "application/javascript" => "js",
        _ => return None,
    })
}

/// Guess an extension from the leading bytes, for files that arrive without a
/// name (a `data:` URL with an unknown media type) or with a useless one.
pub fn sniff_extension(bytes: &[u8]) -> &'static str {
    let head = |n: usize| bytes.get(..n).unwrap_or(bytes);
    if head(8).starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return "png";
    }
    if head(3).starts_with(&[0xff, 0xd8, 0xff]) {
        return "jpg";
    }
    if head(6).starts_with(b"GIF87a") || head(6).starts_with(b"GIF89a") {
        return "gif";
    }
    if head(12).starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        return "webp";
    }
    if head(5).starts_with(b"%PDF-") {
        return "pdf";
    }
    if head(4).starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        return "zip";
    }
    let text = String::from_utf8_lossy(head(512));
    let trimmed = text.trim_start();
    if trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && text.contains("<svg")) {
        return "svg";
    }
    "bin"
}

/// Turn an image `src` (a `data:` URL or a local path) into a downloadable
/// file. `None` when the source cannot be read or is not a local resource.
pub fn asset_from_src(src: &str) -> Option<Download> {
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    if src.starts_with("data:") {
        let (mime, bytes) = decode_data_url(src)?;
        let extension = extension_for_mime(&mime).unwrap_or_else(|| sniff_extension(&bytes));
        return Some(Download {
            name: format!("image.{extension}"),
            bytes,
        });
    }
    let path = src
        .strip_prefix("file://")
        .or_else(|| src.strip_prefix("file.pwb/"))
        .unwrap_or(src);
    let path = Path::new(path);
    if !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("image.{}", sniff_extension(&bytes)));
    Some(Download { name, bytes })
}

/// If `url` names a file rather than a page, the download it should become.
///
/// A `.pwb` target is a page, never a download. `file.pwb/<path>`, `file://`
/// paths, absolute/relative filesystem paths, and `data:` URLs are downloads;
/// unknown extensions are fine (they are saved byte-for-byte).
pub fn download_target(url: &str) -> Option<Download> {
    if !is_download_target(url) {
        return None;
    }
    let url = url.trim();
    if url.starts_with("data:") {
        let (mime, bytes) = decode_data_url(url)?;
        let stem = extension_for_mime(&mime)
            .map(|extension| format!("download.{extension}"))
            .unwrap_or_else(|| format!("download.{}", sniff_extension(&bytes)));
        return Some(Download { name: stem, bytes });
    }
    let path_text = url
        .strip_prefix("file.pwb/")
        .or_else(|| url.strip_prefix("file://"))
        .or_else(|| url.strip_prefix("plain://file.pwb/"))
        .unwrap_or(url);
    let path = Path::new(path_text);
    let bytes = std::fs::read(path).ok()?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("download.{}", sniff_extension(&bytes)));
    Some(Download { name, bytes })
}

/// Whether `url` looks like a file download rather than a page, without
/// touching the filesystem. Used to block page scripts from turning a
/// navigation into a local-file copy; only a user gesture may download.
pub fn is_download_target(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    if url.starts_with("data:") {
        return true;
    }
    let path_text = url
        .strip_prefix("file.pwb/")
        .or_else(|| url.strip_prefix("file://"))
        .or_else(|| url.strip_prefix("plain://file.pwb/"))
        .unwrap_or(url);
    // Only treat filesystem-looking targets as downloads. A bare name like
    // `mysite.pwb` or `search.plain` stays a page lookup.
    let path = Path::new(path_text);
    let looks_like_path = url.starts_with("file.pwb/")
        || url.starts_with("file://")
        || path.is_absolute()
        || path_text.starts_with("./")
        || path_text.starts_with("../");
    looks_like_path
        && !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pwb"))
}

/// Save a `Download` into `dir` (name split into stem + extension).
pub fn save_download(dir: &Path, download: &Download) -> io::Result<PathBuf> {
    let path = Path::new(&download.name);
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|stem| slug(&stem.to_string_lossy()))
        .unwrap_or_else(|| "download".to_string());
    let stem = if stem == "page" {
        "download".to_string()
    } else {
        stem
    };
    write_file(dir, &stem, &extension, &download.bytes)
}

/// Save the current page's bundle plus its images (page, complete).
///
/// `asset_srcs` are the `src` values of the page's `<image>` nodes; unreadable
/// or remote sources are skipped rather than failing the page download.
pub fn save_page_in(
    dir: &Path,
    url: &str,
    title: &str,
    bundle: &[u8],
    asset_srcs: &[String],
) -> io::Result<PageSave> {
    let file = page_name(url, title);
    std::fs::create_dir_all(dir)?;
    let bundle_path = unique_path(
        dir,
        Path::new(&file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .as_ref(),
        "pwb",
    );
    std::fs::write(&bundle_path, bundle)?;

    let mut assets = Vec::new();
    let mut used: HashMap<String, String> = HashMap::new();
    if !asset_srcs.is_empty() {
        let stem = bundle_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "page".to_string());
        let files_dir = dir.join(format!("{stem}_files"));
        let mut counter = 0usize;
        for src in asset_srcs {
            let Some(asset) = asset_from_src(src) else {
                continue;
            };
            // The same image used twice is saved once.
            if used.contains_key(src) {
                continue;
            }
            let path = Path::new(&asset.name);
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().to_string())
                .unwrap_or_else(|| sniff_extension(&asset.bytes).to_string());
            let base = path
                .file_stem()
                .map(|stem| slug(&stem.to_string_lossy()))
                .filter(|stem| stem != "image")
                .unwrap_or_else(|| {
                    counter += 1;
                    format!("image-{counter}")
                });
            if let Ok(written) = write_file(&files_dir, &base, &extension, &asset.bytes) {
                used.insert(src.clone(), written.display().to_string());
                assets.push(written);
            }
        }
    }
    Ok(PageSave {
        bundle: bundle_path,
        assets,
    })
}

/// [`save_page_in`] using the default downloads directory.
pub fn save_page(
    url: &str,
    title: &str,
    bundle: &[u8],
    asset_srcs: &[String],
) -> io::Result<PageSave> {
    save_page_in(&downloads_dir()?, url, title, bundle, asset_srcs)
}

/// [`save_download`] using the default downloads directory.
pub fn save(download: &Download) -> io::Result<PathBuf> {
    save_download(&downloads_dir()?, download)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pwb-downloads-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slugs_are_filesystem_safe_and_never_empty() {
        assert_eq!(slug("My Page: Part 2!"), "my-page-part-2");
        assert_eq!(slug("  ...  "), "page");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
    }

    #[test]
    fn unique_paths_never_overwrite() {
        let dir = temp_dir("unique");
        std::fs::write(dir.join("note.txt"), b"one").unwrap();
        let next = unique_path(&dir, "note", "txt");
        assert_eq!(next.file_name().unwrap(), "note-1.txt");
        std::fs::write(&next, b"two").unwrap();
        assert_eq!(
            unique_path(&dir, "note", "txt").file_name().unwrap(),
            "note-2.txt"
        );
    }

    #[test]
    fn data_urls_decode_and_get_an_extension() {
        // 1x1 transparent PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let asset = asset_from_src(&format!("data:image/png;base64,{png}")).expect("data image");
        assert!(asset.name.ends_with(".png"));
        assert!(asset.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn extensions_are_sniffed_for_unknown_types() {
        assert_eq!(sniff_extension(b"%PDF-1.7\n"), "pdf");
        assert_eq!(sniff_extension(b"GIF89a...."), "gif");
        assert_eq!(sniff_extension(b"\x00\x01\x02\x03"), "bin");
    }

    #[test]
    fn pwb_targets_stay_pages_and_files_download() {
        assert!(download_target("mysite.pwb").is_none());
        assert!(download_target("search.plain?q=x").is_none());
        let dir = temp_dir("target");
        let pdf = dir.join("report.pdf");
        std::fs::write(&pdf, b"%PDF-1.7\n").unwrap();
        let download = download_target(&format!("file.pwb/{}", pdf.display())).expect("pdf");
        assert_eq!(download.name, "report.pdf");
        assert!(is_download_target(&format!("file.pwb/{}", pdf.display())));
        assert!(!is_download_target(&format!(
            "file.pwb/{}.pwb",
            pdf.with_extension("pwb").display()
        )));
    }

    #[test]
    fn saving_a_page_writes_the_bundle_and_its_images() {
        let dir = temp_dir("page");
        let src_dir = temp_dir("page-src");
        let png = src_dir.join("logo.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\nrest").unwrap();

        let saved = save_page_in(
            &dir,
            "mysite.pwb",
            "My Site",
            b"bundle-bytes",
            &[png.display().to_string()],
        )
        .expect("page saved");

        assert_eq!(saved.bundle.file_name().unwrap(), "my-site.pwb");
        assert_eq!(std::fs::read(&saved.bundle).unwrap(), b"bundle-bytes");
        assert_eq!(saved.assets.len(), 1);
        assert_eq!(saved.assets[0].file_name().unwrap(), "logo.png");
        assert_eq!(
            saved.assets[0].parent().unwrap().file_name().unwrap(),
            "my-site_files"
        );
    }
}
