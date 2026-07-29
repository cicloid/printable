//! Resolve markdown image references to printer-ready bitmaps.
//!
//! Core is sans-IO: [`printa_ble_core::raster::markdown_image_refs`] lists the
//! references a document uses, each surface fetches the bytes its own way, and
//! [`printa_ble_core::raster::render_markdown_with`] renders with whatever was
//! resolved. Anything missing from the map renders as an italic placeholder, so
//! a broken image never fails a print.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use printa_ble_core::raster::{markdown_image_refs, Bitmap, Dither};

use crate::print_service::bitmap_from_image_bytes;

/// Give up on a slow server rather than block a print forever.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Refuse oversized downloads: a receipt is 384px wide, nothing legitimate
/// comes close to this.
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Resolve image references in a markdown document to bitmaps.
///
/// `base_dir` is the directory of the source `.md` file; relative local
/// references resolve against it.
///
/// `allow_local` MUST be false for network-facing callers (the server): it is a
/// security boundary preventing LAN clients from reading the server's
/// filesystem. With it false this function performs no filesystem access at all
/// for non-HTTP references — it does not even stat the path.
///
/// Never panics and never fails: unreachable, oversized, or undecodable images
/// warn on stderr and are left out of the map (the document then shows a
/// placeholder in their place).
pub async fn resolve(
    md: &str,
    base_dir: Option<&Path>,
    allow_local: bool,
) -> HashMap<String, Bitmap> {
    let refs = markdown_image_refs(md);
    let mut out = HashMap::new();
    if refs.is_empty() {
        return out;
    }

    // Built once, and only if the document actually references a remote image.
    let client = if refs.iter().any(|dest| is_http(dest)) {
        match build_client() {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("warning: cannot create HTTP client, skipping remote images: {e}");
                None
            }
        }
    } else {
        None
    };

    for dest in refs {
        let bytes = if is_http(&dest) {
            let Some(client) = client.as_ref() else {
                continue;
            };
            match fetch_remote(client, &dest).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: skipping image {dest}: {e}");
                    continue;
                }
            }
        } else if allow_local {
            // CLI only. Reading any path the user can already read is fine here
            // — it is their own shell, their own filesystem.
            match std::fs::read(local_path(&dest, base_dir)) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("warning: skipping image {dest}: {e}");
                    continue;
                }
            }
        } else {
            // SECURITY BOUNDARY: no filesystem access for network-facing
            // callers. Return before touching the path in any way.
            eprintln!("warning: skipping local image {dest}: only http(s) images are allowed here");
            continue;
        };

        match bitmap_from_image_bytes(&bytes, Dither::FloydSteinberg) {
            Ok(bitmap) => {
                out.insert(dest, bitmap);
            }
            Err(e) => eprintln!("warning: skipping image {dest}: {e:#}"),
        }
    }

    out
}

fn is_http(dest: &str) -> bool {
    let lower = dest.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()
}

/// Relative references resolve against the document's directory; absolute ones
/// are used as-is.
fn local_path(dest: &str, base_dir: Option<&Path>) -> PathBuf {
    let path = Path::new(dest);
    match base_dir {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path.to_path_buf(),
    }
}

/// GET `url`, rejecting non-2xx and bodies over [`MAX_IMAGE_BYTES`].
///
/// The size check is belt-and-braces: `Content-Length` is honoured up front when
/// the server sends one, and the body is then read chunk by chunk so a missing
/// or lying header still cannot make us buffer more than the limit.
async fn fetch_remote(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_IMAGE_BYTES {
            anyhow::bail!("image is {len} bytes, over the {MAX_IMAGE_BYTES} byte limit");
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() as u64 + chunk.len() as u64 > MAX_IMAGE_BYTES {
            anyhow::bail!("image is over the {MAX_IMAGE_BYTES} byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use printa_ble_core::raster::bitmap_to_png;

    /// A real, decodable 384-wide PNG.
    fn png_bytes() -> Vec<u8> {
        let mut bitmap = Bitmap::new(20);
        for x in 0..384 {
            bitmap.set(x, 10, true);
        }
        bitmap_to_png(&bitmap)
    }

    #[tokio::test]
    async fn resolves_relative_local_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), png_bytes()).unwrap();

        let images = resolve("![pic](photo.png)", Some(dir.path()), true).await;

        assert_eq!(images.len(), 1, "images: {:?}", images.keys());
        assert!(images["photo.png"].height() > 0);
    }

    #[tokio::test]
    async fn resolves_absolute_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, png_bytes()).unwrap();
        let md = format!("![pic]({})", path.display());

        let images = resolve(&md, None, true).await;

        assert_eq!(images.len(), 1, "images: {:?}", images.keys());
    }

    /// The security boundary: even a file that exists and is readable stays
    /// unread when `allow_local` is false.
    #[tokio::test]
    async fn local_files_are_never_read_when_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, png_bytes()).unwrap();
        let md = format!("![pic]({})\n\n![rel](photo.png)", path.display());

        let images = resolve(&md, Some(dir.path()), false).await;

        assert!(images.is_empty(), "images: {:?}", images.keys());
    }

    #[tokio::test]
    async fn unreachable_url_is_skipped() {
        // Port 1 refuses immediately, so this stays fast.
        let images = resolve("![x](http://127.0.0.1:1/x.png)", None, false).await;
        assert!(images.is_empty(), "images: {:?}", images.keys());
    }

    #[tokio::test]
    async fn undecodable_bytes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.png"), b"not an image at all").unwrap();

        let images = resolve("![pic](photo.png)", Some(dir.path()), true).await;

        assert!(images.is_empty(), "images: {:?}", images.keys());
    }

    #[tokio::test]
    async fn missing_local_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let images = resolve("![pic](nope.png)", Some(dir.path()), true).await;
        assert!(images.is_empty(), "images: {:?}", images.keys());
    }

    #[tokio::test]
    async fn document_without_images_resolves_to_nothing() {
        assert!(resolve("# hi\n\njust text", None, true).await.is_empty());
    }

    #[test]
    fn http_scheme_detection_is_case_insensitive() {
        assert!(is_http("HTTPS://example.com/a.png"));
        assert!(is_http("http://example.com/a.png"));
        assert!(!is_http("ftp://example.com/a.png"));
        assert!(!is_http("/etc/hosts"));
        assert!(!is_http("photo.png"));
    }
}
