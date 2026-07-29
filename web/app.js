// lxd2 web — static Web Bluetooth page. Rendering and the print protocol
// state machine live in WASM (lxd2-core); this file owns the DOM and GATT.

import init, {
  render_text,
  render_markdown,
  render_qr,
  render_image,
  WasmJob,
} from "./pkg/lxd2_web.js";

const SERVICE = 0xffe6;
const WRITE = 0xffe1;
const NOTIFY = 0xffe2;

const DEFAULT_TEXT_SIZE = 24.0; // matches the CLI/server default
const WATCHDOG_MS = 10_000;

const $ = (id) => document.getElementById(id);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const bluetoothSupported = !!navigator.bluetooth;

// --- Connection state ---
let device = null;
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
// Returns a WasmBitmap the CALLER must free().
async function renderCurrent() {
  switch (activeTab) {
    case "text":
      return render_text($("text-content").value, DEFAULT_TEXT_SIZE);
    case "markdown":
      return render_markdown($("md-content").value);
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
      filters: [{ namePrefix: "LX" }],
      optionalServices: [SERVICE],
    });
    device.addEventListener("gattserverdisconnected", onDisconnect);
    const server = await device.gatt.connect();
    const svc = await server.getPrimaryService(SERVICE);
    writeChar = await svc.getCharacteristic(WRITE);
    notifyChar = await svc.getCharacteristic(NOTIFY);
    await notifyChar.startNotifications();
    notifyChar.addEventListener("characteristicvaluechanged", onNotify);
    connected = true;
    updateChip();
  } catch (e) {
    connected = false;
    device = null;
    updateChip();
    $("connect").disabled = false;
    throw e;
  }
}

function onDisconnect() {
  const wasPrinting = job !== null;
  connected = false;
  device = null;
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
function runJob(bitmap, density) {
  return new Promise((resolve, reject) => {
    let j;
    try {
      const challenge = crypto.getRandomValues(new Uint8Array(10));
      j = new WasmJob(bitmap, density, challenge);
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
    // Feed is part of the bitmap, so it repeats per copy (same as the CLI).
    bitmap.extend_blank(optFeed());
    const density = optDensity();
    const copies = optCopies();
    const lines = bitmap.height();
    for (let i = 0; i < copies; i++) {
      await runJob(bitmap, density);
    }
    let msg = "Printed " + lines + " lines";
    if (copies > 1) msg += " × " + copies + " copies";
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

$("preview-btn").disabled = false;
$("print-btn").disabled = !bluetoothSupported;
$("preview-btn").addEventListener("click", doPreview);
$("print-btn").addEventListener("click", doPrint);
$("connect").addEventListener("click", () => {
  connect().catch((e) => toast(errMsg(e), true));
});
