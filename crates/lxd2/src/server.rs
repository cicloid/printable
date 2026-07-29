//! HTTP print server (`lxd2 serve`).
//!
//! Exposes a printa-style REST API on the LAN: health/status plus preview
//! endpoints that render text, markdown, QR codes and images to PNG without
//! touching the printer. Print endpoints build on the same handlers in a
//! later task, serialized through [`AppState::print_lock`].

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lxd2_core::raster::{bitmap_to_png, render_markdown, render_qr, render_text, Dither};
use serde::Deserialize;
use serde_json::json;

use crate::ble;
use crate::config::Config;
use crate::print_service::{self, SCAN_TIMEOUT};

/// Largest accepted request body (image uploads), in bytes.
const BODY_LIMIT: usize = 20 * 1024 * 1024;

/// How long `/status` waits for an unsolicited status frame.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// Default font size for text rendering, matching the CLI default.
const DEFAULT_TEXT_SIZE: f32 = 24.0;

/// Largest accepted font size in pixels.
const MAX_TEXT_SIZE: f32 = 128.0;

/// Shared server state.
pub struct AppState {
    /// `--device` filter given at serve time (overrides the saved device).
    pub device: Option<String>,
    /// Serializes print jobs: one printer, one job at a time. Held across
    /// the whole connect-print-disconnect flow by the print endpoints
    /// (added in a later task); preview endpoints never take it.
    #[allow(dead_code)] // taken by the print endpoints (next task)
    pub print_lock: tokio::sync::Mutex<()>,
}

/// An API error: HTTP status plus a message, rendered as `{"error": msg}`.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Build the application router.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/preview/text", post(preview_text))
        .route("/preview/markdown", post(preview_markdown))
        .route("/preview/qr", post(preview_qr))
        .route("/preview/image", post(preview_image))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Bind and run the server until interrupted.
pub async fn serve(bind: &str, port: u16, device: Option<String>) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        device,
        print_lock: tokio::sync::Mutex::new(()),
    });
    let listener = tokio::net::TcpListener::bind((bind, port))
        .await
        .with_context(|| format!("failed to bind {bind}:{port}"))?;
    let addr = listener.local_addr()?;
    println!("Listening on http://{addr}");
    if bind == "0.0.0.0" {
        if let Some(ip) = lan_ip() {
            println!("On your LAN: http://{ip}:{}", addr.port());
        }
    }
    axum::serve(listener, router(state)).await?;
    Ok(())
}

/// Best-effort LAN address of this machine, for the startup hint.
///
/// Connecting a UDP socket picks the outbound interface without sending
/// any packets.
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

// ---------------------------------------------------------------------------
// Handlers. The render calls below are CPU-bound but fast (< 50 ms for a
// 384-px-wide bitmap), so they run directly in the async handlers — no
// spawn_blocking needed at this scale.
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "url_printing": cfg!(feature = "url"),
    }))
}

/// Connect to the printer, wait for a status frame, disconnect.
async fn status(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        state.device.as_deref(),
        config.device.as_ref(),
        SCAN_TIMEOUT,
    )
    .await
    .map_err(|e| ApiError::unavailable(format!("{e:#}")))?;
    print_service::remember_device(&mut config, &printer);
    let status = printer.wait_status(STATUS_TIMEOUT).await;
    printer.disconnect().await;
    let s = status.map_err(|e| ApiError::unavailable(format!("{e:#}")))?;

    let mut body = json!({
        "battery_pct": s.battery_pct,
        "no_paper": s.no_paper,
        "charging": s.charging,
        "charged": s.charged,
        "overheat": s.overheat,
        "low_battery": s.low_battery,
    });
    if let Some(d) = s.density {
        body["density"] = json!(d);
    }
    if let Some(mv) = s.voltage_mv {
        body["voltage_mv"] = json!(mv);
    }
    Ok(Json(body).into_response())
}

#[derive(Deserialize)]
struct TextBody {
    content: String,
    size: Option<f32>,
}

#[derive(Deserialize)]
struct MarkdownBody {
    content: String,
}

#[derive(Deserialize)]
struct QrBody {
    data: String,
    caption: Option<String>,
}

async fn preview_text(Json(body): Json<TextBody>) -> Result<Response, ApiError> {
    let bitmap = render_text(&body.content, validate_text(&body.content, body.size)?);
    Ok(png_response(bitmap_to_png(&bitmap)))
}

async fn preview_markdown(Json(body): Json<MarkdownBody>) -> Result<Response, ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    Ok(png_response(bitmap_to_png(&render_markdown(&body.content))))
}

async fn preview_qr(Json(body): Json<QrBody>) -> Result<Response, ApiError> {
    let bitmap = render_qr(&body.data, body.caption.as_deref())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(png_response(bitmap_to_png(&bitmap)))
}

/// Multipart: required `file` field (image bytes), optional `dither` field.
async fn preview_image(mut multipart: Multipart) -> Result<Response, ApiError> {
    let mut file: Option<Vec<u8>> = None;
    let mut dither = Dither::FloydSteinberg;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart body: {e}")))?
    {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("file") => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::bad_request(format!("failed to read file: {e}")))?;
                file = Some(bytes.to_vec());
            }
            Some("dither") => {
                let value = field
                    .text()
                    .await
                    .map_err(|e| ApiError::bad_request(format!("failed to read dither: {e}")))?;
                dither = dither_from_str(&value).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "unknown dither `{value}` (expected floyd, atkinson, threshold or none)"
                    ))
                })?;
            }
            _ => {} // ignore unknown fields
        }
    }
    let bytes = file.ok_or_else(|| ApiError::bad_request("missing `file` field"))?;
    let bitmap = print_service::bitmap_from_image_bytes(&bytes, dither)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(png_response(bitmap_to_png(&bitmap)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Validate text content and resolve the font size, mirroring the CLI checks.
fn validate_text(content: &str, size: Option<f32>) -> Result<f32, ApiError> {
    if content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    let size = size.unwrap_or(DEFAULT_TEXT_SIZE);
    if !size.is_finite() || size <= 0.0 || size > MAX_TEXT_SIZE {
        return Err(ApiError::bad_request(format!(
            "size must be greater than 0 and at most {MAX_TEXT_SIZE}"
        )));
    }
    Ok(size)
}

/// Parse a dither name; same aliases as the CLI (`none` = plain threshold).
fn dither_from_str(s: &str) -> Option<Dither> {
    match s {
        "floyd" => Some(Dither::FloydSteinberg),
        "atkinson" => Some(Dither::Atkinson),
        "threshold" | "none" => Some(Dither::Threshold),
        _ => None,
    }
}

/// Wrap PNG bytes in an `image/png` response.
fn png_response(png: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "image/png")], png).into_response()
}

// ---------------------------------------------------------------------------
// Tests. `/status` is deliberately untested here: any request to it scans
// for and connects to a real printer over BLE, which a unit test must not
// do. Its connect/status/disconnect flow is the same code path as the
// hardware-validated `lxd2 status` command.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn app() -> Router {
        router(Arc::new(AppState {
            device: None,
            print_lock: tokio::sync::Mutex::new(()),
        }))
    }

    async fn post_json(uri: &str, body: &str) -> Response {
        app()
            .oneshot(
                Request::post(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn body_bytes(resp: Response) -> Vec<u8> {
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    /// Assert a PNG came back: 200, image/png, PNG file signature.
    async fn assert_png(resp: Response) {
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/png"
        );
        let body = body_bytes(resp).await;
        assert!(body.starts_with(PNG_MAGIC), "body is not a PNG");
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = app()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("ok"), "body: {body}");
        assert!(body.contains("url_printing"), "body: {body}");
    }

    #[tokio::test]
    async fn preview_text_returns_png() {
        assert_png(post_json("/preview/text", r#"{"content":"hello"}"#).await).await;
    }

    #[tokio::test]
    async fn preview_markdown_returns_png() {
        assert_png(post_json("/preview/markdown", r##"{"content":"# Hi\n\n- a\n- b"}"##).await)
            .await;
    }

    #[tokio::test]
    async fn preview_qr_returns_png() {
        assert_png(
            post_json(
                "/preview/qr",
                r#"{"data":"https://example.com","caption":"scan me"}"#,
            )
            .await,
        )
        .await;
    }

    #[tokio::test]
    async fn preview_qr_too_long_is_400() {
        let data = "x".repeat(4000);
        let resp = post_json("/preview/qr", &format!(r#"{{"data":"{data}"}}"#)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("error"), "body: {body}");
    }

    #[tokio::test]
    async fn preview_text_empty_is_400() {
        let resp = post_json("/preview/text", r#"{"content":"  "}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("error"), "body: {body}");
    }

    #[tokio::test]
    async fn preview_text_bad_size_is_400() {
        for body in [
            r#"{"content":"x","size":0}"#,
            r#"{"content":"x","size":1e9}"#,
        ] {
            let resp = post_json("/preview/text", body).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    /// Build a multipart/form-data body by hand with the given text fields
    /// and one `file` field carrying `file_bytes`.
    fn multipart_body(fields: &[(&str, &str)], file_bytes: &[u8]) -> (String, Vec<u8>) {
        let boundary = "lxd2-test-boundary";
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
                )
                .as_bytes(),
            );
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"test.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    async fn post_multipart(fields: &[(&str, &str)], file_bytes: &[u8]) -> Response {
        let (content_type, body) = multipart_body(fields, file_bytes);
        app()
            .oneshot(
                Request::post("/preview/image")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn preview_image_multipart_returns_png() {
        let png = bitmap_to_png(&render_text("img", 24.0));
        assert_png(post_multipart(&[("dither", "atkinson")], &png).await).await;
    }

    #[tokio::test]
    async fn preview_image_bad_dither_is_400() {
        let png = bitmap_to_png(&render_text("img", 24.0));
        let resp = post_multipart(&[("dither", "bogus")], &png).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("dither"), "body: {body}");
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let resp = app()
            .oneshot(Request::get("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
