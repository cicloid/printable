//! Render web pages to PNG using system headless Chrome.
//!
//! Feature-gated behind `url` (on by default); build with
//! `--no-default-features` to drop the Chrome dependency entirely.

use std::time::Duration;

use anyhow::{bail, Context as _};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use futures::StreamExt as _;

/// Viewport used for rendering: printer paper width by a nominal height.
/// The screenshot captures the full page regardless of the height.
const VIEWPORT: (u32, u32) = (384, 800);

/// How long the page gets to settle after navigation before the screenshot,
/// so late-loading fonts/images make it into the capture.
const SETTLE: Duration = Duration::from_millis(500);

/// Accept only `http://` and `https://` URLs (case-insensitive scheme).
///
/// This is a security boundary, not just input validation: the server will
/// expose URL printing on the LAN, and other schemes (`file://`,
/// `javascript:`, `data:`) would let callers read local files or run
/// arbitrary content in the rendering browser.
pub fn validate_url(url: &str) -> anyhow::Result<()> {
    let ok = ["http://", "https://"].iter().any(|p| {
        url.get(..p.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(p))
    });
    if !ok {
        bail!("only http:// and https:// URLs can be printed (got `{url}`)");
    }
    Ok(())
}

/// Render a URL to a full-page PNG at 384 px width using system Chrome.
pub async fn render_url_png(url: &str) -> anyhow::Result<Vec<u8>> {
    validate_url(url)?;

    let config = BrowserConfig::builder()
        .window_size(VIEWPORT.0, VIEWPORT.1)
        .arg("--hide-scrollbars")
        .build()
        .map_err(anyhow::Error::msg)?;

    let (mut browser, mut handler) = Browser::launch(config).await.context(
        "could not launch Chrome — is Google Chrome installed? \
         (build with --no-default-features to disable URL printing)",
    )?;

    // chromiumoxide needs its event loop polled for the connection to work.
    let events = tokio::spawn(async move { while handler.next().await.is_some() {} });

    // Do the actual work in a helper so cleanup below runs on every path.
    let result = screenshot(&browser, url).await;

    if browser.close().await.is_ok() {
        let _ = browser.wait().await;
    }
    events.abort();

    result
}

/// Open `url` in a new page and capture a full-page PNG screenshot.
async fn screenshot(browser: &Browser, url: &str) -> anyhow::Result<Vec<u8>> {
    let page = browser
        .new_page(url)
        .await
        .with_context(|| format!("failed to open {url}"))?;
    page.wait_for_navigation()
        .await
        .with_context(|| format!("failed to load {url}"))?;
    tokio::time::sleep(SETTLE).await;
    page.screenshot(
        ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(true)
            .capture_beyond_viewport(true)
            .build(),
    )
    .await
    .with_context(|| format!("failed to capture screenshot of {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_accepts_http_and_https() {
        for url in [
            "http://example.com",
            "https://example.com",
            "HTTP://EXAMPLE.COM",
            "HtTpS://example.com/path?q=1",
        ] {
            assert!(validate_url(url).is_ok(), "should accept {url}");
        }
    }

    #[test]
    fn validate_url_rejects_other_schemes() {
        for url in [
            "file:///etc/passwd",
            "ftp://host/file",
            "javascript:alert(1)",
            "data:text/html,<h1>hi</h1>",
            "",
            "garbage",
            "https:/example.com",
            "héllo://multibyte",
        ] {
            assert!(validate_url(url).is_err(), "should reject {url}");
        }
    }

    /// Requires Chrome and network; run manually with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires Chrome and network access"]
    async fn render_example_com() {
        let png = render_url_png("https://example.com")
            .await
            .expect("rendering example.com should succeed");
        assert!(png.len() > 8, "PNG should not be empty");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "should be a PNG");
    }
}
