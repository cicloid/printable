// printa-ble web — static Web Bluetooth page. Rendering and the print protocol
// state machine live in WASM (printa-ble-core); this file owns the DOM and GATT.

import init, {
  render_text,
  render_markdown_with_images,
  markdown_image_refs,
  ImageSet,
  render_qr,
  render_image,
  WasmJob,
  WasmX6Job,
  lx_service_uuid,
  lx_write_uuid,
  lx_notify_uuid,
  x6_service_uuid,
  x6_write_uuid,
  x6_notify_uuid,
} from "./pkg/printa_ble_web.js";

// GATT UUIDs come from core's PrinterModel (the single source of truth);
// assigned after init() below, before any button is enabled.
let LX_SERVICE, LX_WRITE, LX_NOTIFY, X6_SERVICE, X6_WRITE, X6_NOTIFY;

const DEFAULT_TEXT_SIZE = 24.0; // matches the CLI/server default
const WATCHDOG_MS = 10_000;
// Markdown images always use Floyd-Steinberg, the CLI and server default. The
// Image tab's dither select applies to that tab's single upload only — a
// markdown document has no per-image control.
const MD_IMAGE_DITHER = "floyd";
// Mirrors MAX_IMAGE_REFS in the CLI/server resolver: a document with hundreds
// of image references would otherwise fetch them all before the preview drew
// anything. References past the cap render as `[image: alt]` placeholders, the
// same as one that fails to load.
const MAX_IMAGE_REFS = 32;

const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const bluetoothSupported = !!navigator.bluetooth;

// --- Connection state ---
let device = null;
let model = null; // "lx" or "x6", set by the service probe in connect()
let writeChar = null;
let notifyChar = null;
let connected = false;
let batteryPct = null; // from unsolicited 5A 02 status frames

// --- Print job state ---
// One job at a time. `job` is the live WasmJob (WASM-owned memory: always
// free()d in finishJob). `jobSettle` resolves/rejects the per-copy promise.
let job = null;
let jobSettle = null;
let isPumping = false;
let watchdog = null;

// wasm-bindgen throws Result<_, String> errors as plain strings, not Errors.
const errMsg = (e) => (e instanceof Error ? e.message : String(e));

// --- Tabs ---
let activeTab = "text";
document.querySelectorAll("#tabs button").forEach((btn) => {
  btn.addEventListener("click", () => {
    activeTab = btn.dataset.tab;
    document.querySelectorAll("#tabs button").forEach((b) =>
      b.classList.toggle("active", b === btn));
    document.querySelectorAll(".tab").forEach((s) =>
      s.classList.toggle("active", s.id === "tab-" + activeTab));
  });
});

// --- Toast / status chip ---
function toast(msg, isError) {
  const t = $("toast");
  t.hidden = !msg;
  t.textContent = msg;
  t.className = isError ? "err" : "ok";
}

function updateChip() {
  const el = $("status");
  if (!bluetoothSupported) {
    el.textContent = "unsupported browser";
  } else if (connected) {
    let text = (device && device.name) || "connected";
    if (batteryPct != null) text += " · battery " + batteryPct + "%";
    el.textContent = text;
  } else {
    el.textContent = "not connected";
  }
}

// --- Options ---
const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, Math.trunc(n) || 0));
const optDensity = () => clamp(Number($("density").value), 1, 7);
const optFeed = () => clamp(Number($("feed").value), 0, 2000);
const optCopies = () => clamp(Number($("copies").value), 1, 20);

$("density").addEventListener("input", () => {
  $("density-val").textContent = $("density").value;
});

// --- Rendering (pure client-side WASM; works without a printer) ---

// Non-fatal note from the last render (currently: markdown images that could
// not be fetched). Set by renderCurrent, reported by the preview/print paths.
let renderNotice = null;

// Render markdown, resolving its image references through the browser.
//
// Core is sans-IO: it tells us which destinations the document uses, we fetch
// what we can, and it renders an italic `[image: alt]` placeholder for
// everything else — a blocked or broken image never fails the document.
// Only http(s) refs are fetchable here: a relative path like `logo.png` names
// a local file the browser has no way to read.
async function renderMarkdown(md) {
  const refs = markdown_image_refs(md);
  const images = new ImageSet();
  // Refs past MAX_IMAGE_REFS are never fetched, but they are still unresolved,
  // so they count toward the same note.
  const overCap = Math.max(0, refs.length - MAX_IMAGE_REFS);
  let skipped = overCap;
  try {
    for (const ref of refs.slice(0, MAX_IMAGE_REFS)) {
      if (!/^https?:\/\//i.test(ref)) {
        skipped++;
        continue;
      }
      try {
        const res = await fetch(ref);
        if (!res.ok) throw new Error("HTTP " + res.status);
        images.add(ref, new Uint8Array(await res.arrayBuffer()), MD_IMAGE_DITHER);
      } catch {
        skipped++; // CORS, network, HTTP error, or undecodable bytes
      }
    }
    if (skipped > 0) {
      renderNotice =
        skipped + (skipped === 1 ? " image" : " images") +
        " could not be loaded (CORS, network, or a local path)";
      if (overCap > 0) {
        renderNotice +=
          " — including " + overCap + " past the " + MAX_IMAGE_REFS +
          "-image limit per document";
      }
    }
    return render_markdown_with_images(md, images);
  } finally {
    images.free(); // the bitmaps are WASM-owned; the render already copied them
  }
}

// Returns a WasmBitmap the CALLER must free().
async function renderCurrent() {
  renderNotice = null;
  switch (activeTab) {
    case "text":
      return render_text($("text-content").value, DEFAULT_TEXT_SIZE);
    case "markdown":
      return await renderMarkdown($("md-content").value);
    case "qr":
      return render_qr($("qr-data").value, $("qr-caption").value || undefined);
    case "image": {
      const file = $("image-file").files[0];
      if (!file) throw new Error("choose an image first");
      const bytes = new Uint8Array(await file.arrayBuffer());
      return render_image(bytes, $("image-dither").value);
    }
  }
}

async function doPreview() {
  toast("", false);
  let bitmap;
  try {
    bitmap = await renderCurrent();
  } catch (e) {
    toast(errMsg(e), true);
    return;
  }
  let png;
  try {
    if (bitmap.height() === 0) {
      toast("nothing to render", true);
      return;
    }
    png = bitmap.to_png();
  } catch (e) {
    toast(errMsg(e), true);
    return;
  } finally {
    bitmap.free(); // preview only needs the PNG bytes
  }
  const img = $("preview-img");
  if (img.src) URL.revokeObjectURL(img.src);
  img.src = URL.createObjectURL(new Blob([png], { type: "image/png" }));
  $("preview-wrap").hidden = false;
  if (renderNotice) toast(renderNotice, true); // rendered, but incomplete
}

// --- Web Bluetooth ---
async function connect() {
  if (!bluetoothSupported) {
    throw new Error("this browser doesn't support Web Bluetooth");
  }
  $("status").textContent = "connecting…";
  $("connect").disabled = true;
  try {
    device = await navigator.bluetooth.requestDevice({
      filters: [{ namePrefix: "LX" }, { namePrefix: "X6h-" }, { namePrefix: "x6h-" }],
      optionalServices: [LX_SERVICE, X6_SERVICE],
    });
    device.addEventListener("gattserverdisconnected", onDisconnect);
    const server = await device.gatt.connect();
    // The exposed primary service IS the model detection: the two families
    // expose disjoint services, so probe LX first and fall back to X6.
    let svc;
    try {
      svc = await server.getPrimaryService(LX_SERVICE);
      model = "lx";
    } catch {
      svc = await server.getPrimaryService(X6_SERVICE);
      model = "x6";
    }
    writeChar = await svc.getCharacteristic(model === "lx" ? LX_WRITE : X6_WRITE);
    notifyChar = await svc.getCharacteristic(model === "lx" ? LX_NOTIFY : X6_NOTIFY);
    await notifyChar.startNotifications();
    notifyChar.addEventListener("characteristicvaluechanged", onNotify);
    connected = true;
    updateChip();
  } catch (e) {
    connected = false;
    device = null;
    model = null;
    updateChip();
    $("connect").disabled = false;
    throw e;
  }
}

function onDisconnect() {
  const wasPrinting = job !== null;
  connected = false;
  device = null;
  model = null;
  writeChar = null;
  notifyChar = null;
  batteryPct = null;
  $("connect").disabled = false;
  updateChip();
  if (wasPrinting) {
    finishJob(new Error("printer disconnected"));
  } else {
    toast("printer disconnected", true);
  }
}

function onNotify(e) {
  const v = e.target.value;
  const bytes = new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
  // Unsolicited status frame (5A 02): battery percentage at byte 2.
  // LX only — the X6 protocol has no battery frame, so the chip never gains one.
  if (bytes.length >= 3 && bytes[0] === 0x5a && bytes[1] === 0x02) {
    batteryPct = bytes[2];
    updateChip();
  }
  clearWatchdog(); // the printer is alive; pump re-arms if it waits again
  if (job) {
    job.on_notification(bytes);
    pump(); // wake the pump if it returned on waitNotification
  }
}

async function gattWrite(bytes) {
  if (writeChar.writeValueWithoutResponse) {
    await writeChar.writeValueWithoutResponse(bytes);
  } else {
    await writeChar.writeValue(bytes);
  }
}

// --- The job pump ---
// Re-entrancy: onNotify calls pump() on every notification, including ones
// that arrive while pump() is awaiting a write or a sleep. The isPumping
// flag makes those calls no-ops; the already-running loop picks up the FSM
// state change on its next next_action() call. Only a pump() that returned
// on waitNotification is actually resumed by onNotify.
async function pump() {
  if (isPumping || !job) return;
  isPumping = true;
  try {
    while (job) {
      const a = job.next_action();
      if (a.kind === "send") {
        await gattWrite(a.bytes);
      } else if (a.kind === "waitMs") {
        await sleep(a.ms);
      } else if (a.kind === "waitNotification") {
        armWatchdog(); // onNotify clears it and re-enters pump
        return;
      } else {
        // done — finishJob checks job.error()
        finishJob(null);
        return;
      }
    }
  } catch (e) {
    finishJob(e);
  } finally {
    isPumping = false;
  }
}

function armWatchdog() {
  clearWatchdog();
  watchdog = setTimeout(() => {
    watchdog = null;
    finishJob(new Error("printer stopped responding"));
  }, WATCHDOG_MS);
}

function clearWatchdog() {
  if (watchdog !== null) {
    clearTimeout(watchdog);
    watchdog = null;
  }
}

// Tear down the current job (success, failure, watchdog, or disconnect) and
// settle its promise. Frees the WasmJob's WASM memory. Idempotent: late
// notifications and stale watchdogs find job === null and do nothing.
function finishJob(err) {
  clearWatchdog();
  const j = job;
  const settle = jobSettle;
  job = null;
  jobSettle = null;
  if (!j) return;
  const jobErr = err || (j.error() ? new Error(j.error()) : null);
  j.free();
  if (!settle) return;
  if (jobErr) settle.reject(jobErr);
  else settle.resolve();
}

// Run one print job (one copy) to completion. The bitmap is only borrowed
// by the WasmJob constructor (the job copies what it needs), so the same
// WasmBitmap is safely reused across copies and freed by the caller.
function runJob(bitmap, density, feed) {
  return new Promise((resolve, reject) => {
    let j;
    try {
      if (model === "x6") {
        // No auth. Density maps to the X6's feed-speed and printhead-energy
        // commands, and feed rides as a pixel-count command, not blank lines.
        j = new WasmX6Job(bitmap, density, feed);
      } else {
        const challenge = crypto.getRandomValues(new Uint8Array(10));
        j = new WasmJob(bitmap, density, challenge);
      }
    } catch (e) {
      reject(new Error(errMsg(e)));
      return;
    }
    job = j;
    jobSettle = { resolve, reject };
    pump();
  });
}

async function doPrint() {
  toast("", false);
  setBusy(true);
  let bitmap = null;
  try {
    try {
      bitmap = await renderCurrent();
    } catch (e) {
      toast(errMsg(e), true);
      return;
    }
    if (!connected) await connect();
    // Snapshot the options once, so editing the form mid-print cannot change
    // later copies of the same job.
    const density = optDensity();
    const feed = optFeed();
    const copies = optCopies();
    // On the LX the feed is part of the bitmap, so it repeats per copy (same
    // as the CLI). On the X6 it is a printer command instead — runJob passes
    // it to the job, so it must not also be baked in here.
    if (model !== "x6") bitmap.extend_blank(feed);
    const lines = bitmap.height();
    for (let i = 0; i < copies; i++) {
      await runJob(bitmap, density, feed);
    }
    let msg = "Printed " + lines + " lines";
    if (copies > 1) msg += " × " + copies + " copies";
    if (renderNotice) msg += " · " + renderNotice;
    toast(msg, false);
  } catch (e) {
    toast(errMsg(e), true);
  } finally {
    if (bitmap) bitmap.free();
    setBusy(false);
  }
}

function setBusy(busy) {
  $("preview-btn").disabled = busy;
  $("print-btn").disabled = busy || !bluetoothSupported;
}

// --- Boot ---
if (!bluetoothSupported) {
  $("banner").hidden = false;
  $("connect").disabled = true;
}
updateChip();

try {
  await init();
} catch (e) {
  toast("failed to load WASM: " + errMsg(e), true);
  throw e;
}

LX_SERVICE = lx_service_uuid();
LX_WRITE = lx_write_uuid();
LX_NOTIFY = lx_notify_uuid();
X6_SERVICE = x6_service_uuid();
X6_WRITE = x6_write_uuid();
X6_NOTIFY = x6_notify_uuid();

$("preview-btn").disabled = false;
$("print-btn").disabled = !bluetoothSupported;
$("preview-btn").addEventListener("click", doPreview);
$("print-btn").addEventListener("click", doPrint);
$("connect").addEventListener("click", () => {
  connect().catch((e) => toast(errMsg(e), true));
});
