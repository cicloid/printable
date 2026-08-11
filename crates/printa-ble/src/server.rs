//! HTTP print server (`printable serve`).
//!
//! Exposes a printa-style REST API on the LAN: health/status, preview
//! endpoints that render text, markdown, QR codes and images to PNG without
//! touching the printer, and print endpoints that run the same rendering
//! through the shared print pipeline, serialized by [`AppState::print_lock`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::extract::{DefaultBodyLimit, Multipart, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use printa_ble_core::model::PrinterModel;
use printa_ble_core::protocol::job::JobError;
use printa_ble_core::raster::{
    bitmap_to_png, render_markdown_with, render_qr, render_text, Bitmap, Dither,
};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::ble;
use crate::config::Config;
use crate::md_images;
use crate::print_service::{
    self, NoPaper, NoPrinterFound, PrintFailure, PrintOptions, PrinterNotResponding, SCAN_TIMEOUT,
};

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
    /// `--model` restriction given at serve time. Like `--device`, this is
    /// server configuration, not per-request: no HTTP route takes a model.
    /// `None` means detect from the saved device or the advertised name.
    pub model: Option<PrinterModel>,
    /// Serializes print jobs: one printer, one job at a time. Held across
    /// the whole connect-print-disconnect flow by the print endpoints, so
    /// concurrent print requests queue (no explicit timeout — the BLE layer
    /// has its own). `/status` only try-locks it; previews never take it.
    pub print_lock: tokio::sync::Mutex<()>,
    /// Whether markdown may pull in http(s) images. `printable serve
    /// --no-remote-images` clears it, leaving the server with no outbound
    /// request surface at all. Local file references are refused regardless.
    pub remote_images: bool,
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

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

/// Print job knobs shared by every print endpoint, embedded in request
/// bodies via `#[serde(flatten)]`. Missing fields take the defaults below.
#[derive(Clone, Copy, Deserialize)]
#[serde(default)]
pub struct PrintOpts {
    pub density: u8,
    pub feed: usize,
    pub copies: u16,
}

impl Default for PrintOpts {
    fn default() -> Self {
        Self {
            density: 3,
            feed: 40,
            copies: 1,
        }
    }
}

impl PrintOpts {
    /// Range-check the options; called by every print handler before
    /// rendering or taking the print lock.
    fn validate(&self) -> Result<(), ApiError> {
        if !(1..=7).contains(&self.density) {
            return Err(ApiError::bad_request("density must be between 1 and 7"));
        }
        if !(1..=20).contains(&self.copies) {
            return Err(ApiError::bad_request("copies must be between 1 and 20"));
        }
        if self.feed > 2000 {
            return Err(ApiError::bad_request("feed must be at most 2000"));
        }
        Ok(())
    }
}

impl From<PrintOpts> for PrintOptions {
    fn from(o: PrintOpts) -> Self {
        Self {
            density: o.density,
            feed: o.feed,
            copies: o.copies,
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
    let router = Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/preview/text", post(preview_text))
        .route("/preview/markdown", post(preview_markdown))
        .route("/preview/qr", post(preview_qr))
        .route("/preview/image", post(preview_image))
        .route("/print/text", post(print_text))
        .route("/print/markdown", post(print_markdown))
        .route("/print/qr", post(print_qr))
        .route("/print/image", post(print_image));
    // Without the `url` feature the routes do not exist (404); /health
    // advertises the capability as `url_printing`.
    #[cfg(feature = "url")]
    let router = router
        .route("/preview/url", post(preview_url))
        .route("/print/url", post(print_url));
    router
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

/// Log one line per request: method, path, status and elapsed milliseconds.
///
/// Server errors log at warn so a default-level (`warn`) server still records
/// its own failures; everything else needs `-v`.
async fn log_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let started = Instant::now();

    let response = next.run(req).await;

    let status = response.status().as_u16();
    let ms = started.elapsed().as_millis();
    if response.status().is_server_error() {
        warn!("{method} {path} -> {status} in {ms}ms");
    } else {
        info!("{method} {path} -> {status} in {ms}ms");
    }
    response
}

/// Bind and run the server until interrupted.
pub async fn serve(
    bind: &str,
    port: u16,
    device: Option<String>,
    model: Option<PrinterModel>,
    remote_images: bool,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        device,
        model,
        print_lock: tokio::sync::Mutex::new(()),
        remote_images,
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

/// Serve the embedded web UI (a single self-contained HTML file).
async fn index() -> Html<&'static str> {
    Html(include_str!("server/ui.html"))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "url_printing": cfg!(feature = "url"),
    }))
}

/// Connect to the printer, wait for a status frame, disconnect.
///
/// If a print job holds the lock, don't queue behind it (a long print would
/// stall this request) and don't open a second BLE connection — report
/// `{"printing": true}` with no other fields instead.
async fn status(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let Ok(_guard) = state.print_lock.try_lock() else {
        debug!("/status: a print job holds the printer, reporting busy");
        return Ok(Json(json!({ "printing": true })).into_response());
    };
    let mut config = Config::load();
    let mut printer = ble::connect_resolved(
        state.device.as_deref(),
        config.device.as_ref(),
        state.model,
        SCAN_TIMEOUT,
    )
    .await
    .map_err(|e| {
        warn!("/status: cannot reach the printer: {e:#}");
        ApiError::unavailable(format!("{e:#}"))
    })?;
    print_service::remember_device(&mut config, &printer);
    let status = printer.wait_status(STATUS_TIMEOUT).await;
    printer.disconnect().await;
    let s = status.map_err(|e| {
        warn!("/status: no status frame from the printer: {e:#}");
        ApiError::unavailable(format!("{e:#}"))
    })?;
    debug!(
        "/status: battery {}%, paper {}, overheat {}",
        s.battery_pct,
        if s.no_paper { "out" } else { "ok" },
        s.overheat
    );

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

/// Fetch the images a markdown document references, for the HTTP surface.
///
/// SECURITY BOUNDARY: `allow_local = false`. The server is reachable from the
/// LAN, so a caller must never be able to make it read its own filesystem —
/// `![x](/etc/hosts)` renders a placeholder, not the file. `base_dir` is `None`
/// for the same reason: a posted document has no directory of its own.
/// Remote http(s) images are fetched unless `--no-remote-images` was given.
async fn resolve_images(state: &AppState, md: &str) -> HashMap<String, Bitmap> {
    let started = Instant::now();
    let images = md_images::resolve(md, None, false, state.remote_images).await;
    if !images.is_empty() {
        debug!(
            "resolved {} markdown images in {}ms",
            images.len(),
            started.elapsed().as_millis()
        );
    }
    images
}

async fn preview_markdown(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MarkdownBody>,
) -> Result<Response, ApiError> {
    if body.content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    let images = resolve_images(&state, &body.content).await;
    let bitmap = render_markdown_with(&body.content, &images);
    Ok(png_response(bitmap_to_png(&bitmap)))
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
                let value = text_field(field, "dither").await?;
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

#[cfg(feature = "url")]
#[derive(Deserialize)]
struct UrlBody {
    url: String,
}

/// Render a URL through headless Chrome to a preview PNG: same pipeline as
/// `/print/url` (screenshot → dithered bitmap) minus the printer.
#[cfg(feature = "url")]
async fn preview_url(Json(body): Json<UrlBody>) -> Result<Response, ApiError> {
    // Scheme check up front: a bad URL must fail before Chrome launches.
    crate::chrome::validate_url(&body.url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let png = render_url(&body.url).await?;
    let bitmap = print_service::bitmap_from_image_bytes(&png, Dither::FloydSteinberg)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(png_response(bitmap_to_png(&bitmap)))
}

/// Screenshot `url` through headless Chrome, logging how long it took.
///
/// Chrome launch plus page load is often the slowest part of a URL print and
/// happens before the printer is ever touched, so it gets its own timing.
#[cfg(feature = "url")]
async fn render_url(url: &str) -> Result<Vec<u8>, ApiError> {
    info!("rendering {url} with headless Chrome");
    let started = Instant::now();
    let png = crate::chrome::render_url_png(url).await.map_err(|e| {
        warn!(
            "Chrome failed to render {url} after {}ms: {e:#}",
            started.elapsed().as_millis()
        );
        ApiError {
            status: StatusCode::BAD_GATEWAY,
            message: format!("failed to render URL: {e:#}"),
        }
    })?;
    info!(
        "rendered {url} to {} KiB in {}ms",
        png.len() / 1024,
        started.elapsed().as_millis()
    );
    Ok(png)
}

// ---------------------------------------------------------------------------
// Print endpoints. Shared shape: validate options and content first (no test
// may reach BLE/Chrome — validation gates before the lock), render exactly
// like the matching preview endpoint, then take the print lock and hand the
// bitmap to the shared print pipeline.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TextPrintBody {
    content: String,
    size: Option<f32>,
    #[serde(flatten)]
    opts: PrintOpts,
}

#[derive(Deserialize)]
struct MarkdownPrintBody {
    content: String,
    #[serde(flatten)]
    opts: PrintOpts,
}

#[derive(Deserialize)]
struct QrPrintBody {
    data: String,
    caption: Option<String>,
    #[serde(flatten)]
    opts: PrintOpts,
}

#[cfg(feature = "url")]
#[derive(Deserialize)]
struct UrlPrintBody {
    url: String,
    #[serde(flatten)]
    opts: PrintOpts,
}

async fn print_text(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TextPrintBody>,
) -> Result<Response, ApiError> {
    body.opts.validate()?;
    let bitmap = render_text(&body.content, validate_text(&body.content, body.size)?);
    print_and_respond(&state, "text", bitmap, body.opts).await
}

async fn print_markdown(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MarkdownPrintBody>,
) -> Result<Response, ApiError> {
    body.opts.validate()?;
    if body.content.trim().is_empty() {
        return Err(ApiError::bad_request("content must not be empty"));
    }
    let images = resolve_images(&state, &body.content).await;
    let bitmap = render_markdown_with(&body.content, &images);
    print_and_respond(&state, "markdown", bitmap, body.opts).await
}

async fn print_qr(
    State(state): State<Arc<AppState>>,
    Json(body): Json<QrPrintBody>,
) -> Result<Response, ApiError> {
    body.opts.validate()?;
    let bitmap = render_qr(&body.data, body.caption.as_deref())
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    print_and_respond(&state, "qr", bitmap, body.opts).await
}

/// Multipart like `/preview/image`, plus optional `density`, `feed` and
/// `copies` text fields.
async fn print_image(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let mut file: Option<Vec<u8>> = None;
    let mut dither = Dither::FloydSteinberg;
    let mut opts = PrintOpts::default();
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
                let value = text_field(field, "dither").await?;
                dither = dither_from_str(&value).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "unknown dither `{value}` (expected floyd, atkinson, threshold or none)"
                    ))
                })?;
            }
            Some("density") => opts.density = parse_field(field, "density").await?,
            Some("feed") => opts.feed = parse_field(field, "feed").await?,
            Some("copies") => opts.copies = parse_field(field, "copies").await?,
            _ => {} // ignore unknown fields
        }
    }
    opts.validate()?;
    let bytes = file.ok_or_else(|| ApiError::bad_request("missing `file` field"))?;
    let bitmap = print_service::bitmap_from_image_bytes(&bytes, dither)
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    print_and_respond(&state, "image", bitmap, opts).await
}

/// Render a URL through headless Chrome, then print it.
#[cfg(feature = "url")]
async fn print_url(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UrlPrintBody>,
) -> Result<Response, ApiError> {
    body.opts.validate()?;
    // Scheme check up front: a bad URL must fail before Chrome or BLE.
    crate::chrome::validate_url(&body.url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let png = render_url(&body.url).await?;
    let bitmap = print_service::bitmap_from_image_bytes(&png, Dither::FloydSteinberg)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    print_and_respond(&state, "url", bitmap, body.opts).await
}

/// Take the print lock, run the shared print pipeline, report what printed.
///
/// The lock is held across the whole connect-print-disconnect flow, so
/// concurrent print requests queue rather than fighting over the printer.
///
/// `kind` names the content for the log — the request line alone cannot say
/// whether `/print/markdown` was two lines or two thousand.
async fn print_and_respond(
    state: &AppState,
    kind: &str,
    bitmap: Bitmap,
    opts: PrintOpts,
) -> Result<Response, ApiError> {
    info!(
        "print job starting: {kind}, {} lines, density {}, feed {}, {} copies",
        bitmap.height(),
        opts.density,
        opts.feed,
        opts.copies
    );

    let _guard = acquire_print_lock(state).await;
    let outcome =
        print_service::print_bitmap(bitmap, state.device.as_deref(), state.model, opts.into())
            .await
            .map_err(|e| {
                let api = print_error_to_api(&e);
                warn!("print job failed ({}): {e:#}", api.status.as_u16());
                api
            })?;

    let stats = outcome.stats;
    info!(
        "print job done: {kind}, {} lines in {}ms ({} packets, {} holds, {} cooldowns, \
         {} resends)",
        outcome.lines,
        outcome.elapsed.as_millis(),
        stats.packets_sent,
        stats.holds,
        stats.cooldowns,
        stats.retransmits,
    );

    // The same diagnostics the log carries, for clients that never see it.
    Ok(Json(json!({
        "printed_lines": outcome.lines,
        "copies": opts.copies,
        "elapsed_ms": outcome.elapsed.as_millis(),
        "packets_sent": stats.packets_sent,
        "holds": stats.holds,
        "cooldowns": stats.cooldowns,
        "retransmits": stats.retransmits,
    }))
    .into_response())
}

/// Take the print lock, saying so when the caller has to queue.
///
/// A request stuck behind another job is indistinguishable from a hang at the
/// client end, so it is logged going in and again with the wait on the way
/// out. There is no timeout: the printer takes as long as it takes.
async fn acquire_print_lock(state: &AppState) -> tokio::sync::MutexGuard<'_, ()> {
    if let Ok(guard) = state.print_lock.try_lock() {
        return guard;
    }
    info!("printer is busy with another job; this request is queued");
    let waited = Instant::now();
    let guard = state.print_lock.lock().await;
    info!(
        "printer free; queued for {}ms",
        waited.elapsed().as_millis()
    );
    guard
}

/// Map a print pipeline error to an API error by downcasting the marker
/// types, mirroring the CLI's `exit_code`.
///
/// Order matters: `NoPaper` is a root cause that context wrappers (like
/// `PrintFailure`) may be layered on top of, so the more specific markers
/// are checked before the generic print-failure context.
fn print_error_to_api(e: &anyhow::Error) -> ApiError {
    if e.downcast_ref::<NoPrinterFound>().is_some()
        || e.downcast_ref::<PrinterNotResponding>().is_some()
    {
        ApiError::unavailable(format!("{e:#}"))
    } else if e.downcast_ref::<NoPaper>().is_some() {
        ApiError::conflict("printer is out of paper")
    } else if e.downcast_ref::<PrintFailure>().is_some() {
        ApiError::internal(format!("{e:#}"))
    } else if matches!(
        e.downcast_ref::<JobError>(),
        Some(JobError::TooLarge { .. })
    ) {
        ApiError::bad_request(format!("{e:#}"))
    } else {
        ApiError::internal(format!("{e:#}"))
    }
}

/// Read a multipart text field, 400 on failure.
async fn text_field(
    field: axum::extract::multipart::Field<'_>,
    name: &str,
) -> Result<String, ApiError> {
    field
        .text()
        .await
        .map_err(|e| ApiError::bad_request(format!("failed to read {name}: {e}")))
}

/// Read a multipart text field and parse it, 400 on failure.
async fn parse_field<T: std::str::FromStr>(
    field: axum::extract::multipart::Field<'_>,
    name: &str,
) -> Result<T, ApiError> {
    let value = text_field(field, name).await?;
    value
        .parse()
        .map_err(|_| ApiError::bad_request(format!("invalid {name} `{value}`")))
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
// Tests. `/status` with a free lock and successful `/print/*` requests are
// deliberately untested here: they scan for and connect to a real printer
// over BLE, which a unit test must not do. Those flows are the same code
// paths as the hardware-validated `printable status` / `printable print` commands.
// The tests below only exercise what runs before BLE: validation, error
// mapping, and the busy branch of `/status`.
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
            model: None,
            print_lock: tokio::sync::Mutex::new(()),
            remote_images: true,
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
    async fn root_serves_ui() {
        let resp = app()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp.headers().get(header::CONTENT_TYPE).unwrap().clone();
        assert!(
            content_type.to_str().unwrap().starts_with("text/html"),
            "content-type: {content_type:?}"
        );
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(
            body.contains("printa-ble"),
            "UI page should mention printa-ble"
        );
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

    /// Security boundary: a local path in a posted document must not be read.
    /// It renders as a placeholder, so the response is a normal 200 PNG.
    #[tokio::test]
    async fn preview_markdown_ignores_local_image_paths() {
        assert_png(
            post_json(
                "/preview/markdown",
                r##"{"content":"![x](/etc/hosts)\n\ntext"}"##,
            )
            .await,
        )
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
        let boundary = "printable-test-boundary";
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

    /// The request log wraps every route, so it must be transparent: a 404
    /// stays a 404 and a PNG body arrives intact through the layer. Both are
    /// covered by every other test in this module going through `router()`;
    /// this one pins the failure path explicitly.
    #[tokio::test]
    async fn request_logging_layer_preserves_error_responses() {
        let resp = post_json("/print/text", r#"{"content":"x","density":9}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("density"), "body: {body}");
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let resp = app()
            .oneshot(Request::get("/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // Print endpoints: validation paths only. No test below may reach BLE or
    // Chrome — every handler validates (and /status try-locks) before any
    // connect, which is what these tests pin down.
    // -----------------------------------------------------------------------

    #[test]
    fn print_opts_validate_defaults_ok() {
        assert!(PrintOpts::default().validate().is_ok());
    }

    #[test]
    fn print_opts_validate_rejects_out_of_range() {
        let ok = PrintOpts::default();
        for (opts, what) in [
            (PrintOpts { density: 0, ..ok }, "density 0"),
            (PrintOpts { density: 8, ..ok }, "density 8"),
            (PrintOpts { copies: 0, ..ok }, "copies 0"),
            (PrintOpts { copies: 21, ..ok }, "copies 21"),
            (PrintOpts { feed: 2001, ..ok }, "feed 2001"),
        ] {
            assert!(opts.validate().is_err(), "{what} should be rejected");
        }
    }

    #[test]
    fn print_opts_validate_accepts_bounds() {
        let ok = PrintOpts::default();
        for opts in [
            PrintOpts { density: 1, ..ok },
            PrintOpts { density: 7, ..ok },
            PrintOpts { copies: 20, ..ok },
            PrintOpts { feed: 2000, ..ok },
            PrintOpts { feed: 0, ..ok },
        ] {
            assert!(opts.validate().is_ok());
        }
    }

    /// Options are validated before render/lock/connect, so an out-of-range
    /// density fails fast without any BLE attempt.
    #[tokio::test]
    async fn density_out_of_range_is_400() {
        let resp = post_json("/print/text", r#"{"content":"x","density":9}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("density"), "body: {body}");
    }

    #[tokio::test]
    async fn print_text_empty_is_400() {
        let resp = post_json("/print/text", r#"{"content":"  "}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("error"), "body: {body}");
    }

    /// Scheme validation runs before Chrome is launched.
    #[cfg(feature = "url")]
    #[tokio::test]
    async fn preview_url_bad_scheme_is_400() {
        let resp = post_json("/preview/url", r#"{"url":"file:///etc/passwd"}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("http"), "body: {body}");
    }

    /// Scheme validation runs before Chrome is launched (and before BLE).
    #[cfg(feature = "url")]
    #[tokio::test]
    async fn print_url_bad_scheme_is_400() {
        let resp = post_json("/print/url", r#"{"url":"file:///etc/passwd"}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert!(body.contains("http"), "body: {body}");
    }

    /// While a print job holds the lock, `/status` must not queue behind it
    /// (or open a second BLE connection): try_lock fails and the handler
    /// returns immediately, before any connect attempt.
    #[tokio::test]
    async fn status_busy_returns_printing() {
        let state = Arc::new(AppState {
            device: None,
            model: None,
            print_lock: tokio::sync::Mutex::new(()),
            remote_images: true,
        });
        let _guard = state.print_lock.lock().await;
        let resp = router(state.clone())
            .oneshot(Request::get("/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(body_bytes(resp).await).unwrap();
        assert_eq!(body, r#"{"printing":true}"#);
    }

    /// A free lock is taken without going down the queueing path at all.
    #[tokio::test]
    async fn print_lock_is_taken_immediately_when_free() {
        let state = AppState {
            device: None,
            model: None,
            print_lock: tokio::sync::Mutex::new(()),
            remote_images: true,
        };
        let guard = tokio::time::timeout(Duration::from_millis(100), acquire_print_lock(&state))
            .await
            .expect("an uncontended lock must not block");
        drop(guard);
    }

    /// The queueing case: a second print request waits for the running job
    /// instead of failing or racing it. This is the wait that looks like a
    /// hang from the client end, which is why it is logged.
    #[tokio::test]
    async fn print_lock_queues_behind_a_running_job() {
        let state = Arc::new(AppState {
            device: None,
            model: None,
            print_lock: tokio::sync::Mutex::new(()),
            remote_images: true,
        });

        let held = state.print_lock.lock().await;
        let waiter = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                let _guard = acquire_print_lock(&state).await;
            }
        });

        // While the first job holds the printer, the second cannot proceed.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), waiter)
                .await
                .is_err(),
            "second request must queue, not barge in"
        );
        drop(held);
    }

    /// Errors synthesized exactly like the production code constructs them.
    #[test]
    fn print_errors_map_to_statuses() {
        use crate::print_service::{NoPaper, NoPrinterFound, PrintFailure};
        use printa_ble_core::protocol::job::JobError;

        // ble.rs: anyhow::Error::msg(NoPrinterFound)
        let e = anyhow::Error::msg(NoPrinterFound);
        assert_eq!(
            print_error_to_api(&e).status,
            StatusCode::SERVICE_UNAVAILABLE
        );

        // ble.rs: the liveness probe found the device but it never answered.
        let e = anyhow::Error::msg(crate::print_service::PrinterNotResponding::new("LX-D02"));
        let api = print_error_to_api(&e);
        assert_eq!(api.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(api.message.contains("LX-D02"), "{}", api.message);

        // print_service.rs: anyhow::Error::msg(NoPaper)
        let e = anyhow::Error::msg(NoPaper);
        assert_eq!(print_error_to_api(&e).status, StatusCode::CONFLICT);

        // print_service.rs: run_job error wrapped with .context(PrintFailure)
        let e = anyhow::anyhow!("write failed").context(PrintFailure);
        assert_eq!(
            print_error_to_api(&e).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );

        // print_service.rs: PrintJob::new error + .context("cannot print…")
        let e = anyhow::Error::from(JobError::TooLarge { packets: 70_000 })
            .context("cannot print this job");
        assert_eq!(print_error_to_api(&e).status, StatusCode::BAD_REQUEST);

        // Anything else stays a plain 500.
        let e = anyhow::anyhow!("boom");
        assert_eq!(
            print_error_to_api(&e).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// Downcast order: with both markers in the chain, the root cause
    /// (NoPaper) must win over the PrintFailure context wrapper.
    #[test]
    fn print_error_no_paper_wins_over_print_failure() {
        use crate::print_service::{NoPaper, PrintFailure};
        let e = anyhow::Error::msg(NoPaper).context(PrintFailure);
        assert_eq!(print_error_to_api(&e).status, StatusCode::CONFLICT);
    }
}
