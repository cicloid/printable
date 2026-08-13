# Security Policy

## Supported Versions

printa-ble is pre-1.0. Only the current `main` branch is supported; fixes land
there and are not backported.

| Version        | Supported          |
| -------------- | ------------------ |
| `main` (0.1.x) | :white_check_mark: |
| Older tags     | :x:                |

## Reporting a Vulnerability

1. **Do NOT** open a public GitHub issue for a security vulnerability.
2. Email the maintainer directly: **security+printable@42fu.com**.
3. Include:
   - A description of the vulnerability and what an attacker gains
   - Steps to reproduce, ideally a minimal request or input
   - The affected surface (CLI, `printable serve`, the web app, the BLE
     protocol) and the commit you tested
   - Any suggested fix (optional)

### What to Expect

- Acknowledgment within **48-168 hours**
- A status update within **7-21 days**
- Fix timeline depends on severity

This is a small hobby project maintained by one person. Please be patient, and
please do not test against machines you do not own.

## Trust Model

**Read this section before deploying anything.** printa-ble's threat model is
narrow on purpose, and several of its behaviors are deliberate trade-offs rather
than oversights. Knowing which is which is the difference between a safe
deployment and an open door.

The short version: **`printable serve` is a LAN appliance for a network you
trust.** It is not hardened for the open internet, and it is not trying to be.

### The Server Has No Authentication

`printable serve` has **no authentication, no authorization, and no rate
limiting**. Anyone who can reach the port can print, read printer status, and
make the server perform outbound HTTP requests.

It binds **`0.0.0.0:8000` by default** — every interface, which is the point on
a home LAN where you want to print from your phone, and exactly wrong anywhere
else.

To restrict it to the local machine:

```bash
printable serve --bind 127.0.0.1
```

The worst case on a trusted LAN is usually wasted paper. The outbound-request
surface below is the part that deserves real thought.

### SSRF: The Server Makes Requests On A Caller's Behalf

Two features cause the server to fetch caller-supplied URLs.

**Markdown images.** `/print/markdown` and `/preview/markdown` fetch any
`http(s)` URL the document references. An unauthenticated caller can therefore
make the server issue GET requests to hosts _it_ can reach and _they_ cannot —
internal services, admin interfaces, cloud instance-metadata endpoints. There is
no allowlist and no private-IP filter. These requests identify themselves with
a `printable/<version>` User-Agent (some hosts, Wikimedia among them, reject
anonymous requests), so a fetched host learns what software asked and its
version.

**URL printing.** `/print/url` and `/preview/url` render a caller-supplied page
through headless Google Chrome. That is the same exposure plus a full browser
engine — JavaScript executes, redirects are followed, and the page can reach
whatever the host can reach.

**Mitigations:**

| Mitigation                           | Effect                                                                                                                                                                       |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `printable serve --no-remote-images` | Removes markdown image fetching entirely. Runtime flag.                                                                                                                      |
| Build with `--no-default-features`   | Removes the `url` feature: `/print/url` and `/preview/url` return 404, and `/health` reports `"url_printing": false`. Compile-time, so it cannot be re-enabled by a request. |
| `--bind 127.0.0.1`                   | Removes remote callers altogether.                                                                                                                                           |

**What does _not_ leak.** The exfiltration channel is narrower than it first
looks, and it is worth being precise:

- A fetched response that is not a decodable PNG or JPEG **fails to decode and
  is dropped**. It never reaches the output. An unresolved reference renders as
  an italic `[image: alt]` placeholder, which tells the caller only that the
  fetch failed — not what came back.
- A response that _is_ a decodable image renders as a **1-bit, 384 px-wide,
  dithered raster**. That is a lossy, low-fidelity channel, not a copy of the
  response body.
- So an attacker can use the server as a **blind-ish port and host scanner**
  (distinguishing "decodable image" from "anything else" from "connection
  failed") and can exfiltrate the coarse visual content of internal images. They
  cannot read internal JSON, HTML, or text through this path.

URL printing via Chrome is a wider channel than the image path — a rendered page
_is_ a screenshot of the response — which is why it is a compile-time feature.

### Local Filesystem: The Server Never Reads Local Paths

When the server resolves markdown image references, it does so with
`allow_local = false`. Local filesystem paths in `![alt](dest)` are **always
refused**, with or without `--no-remote-images`.

This is a deliberate, tested security boundary. Without it, any caller could
read files off the host with `![x](/etc/passwd)` — and, thanks to the dithering
pipeline, get back a picture of them.

The **CLI does read local paths**, relative to the markdown file's own
directory. That is by design and is not a vulnerability: someone running
`printable print -f notes.md` already owns the filesystem they are reading from.
The distinction is exactly the caller's trust level, and it is enforced at the
one place the trust level differs.

### Resource Bounds

The server applies these limits. They bound accidental damage and casual abuse;
they are not a defense against a determined attacker who can already reach the
port.

| Limit                         | Value          | Scope                                                  |
| ----------------------------- | -------------- | ------------------------------------------------------ |
| Request body size             | **20 MB**      | Every route (`DefaultBodyLimit`)                       |
| Image references per document | **32**         | Markdown rendering; the rest render as placeholders    |
| Total image-resolution budget | **30 s**       | Per document, CLI and server                           |
| Per-image fetch size          | **5 MB**       | Rejected on `Content-Length` and again while streaming |
| Per-image fetch timeout       | **15 s**       | Each remote fetch                                      |
| Concurrent print jobs         | **1**          | Serialized by a mutex; further requests queue          |
| Font size                     | **128 px** max | Text rendering                                         |
| Copies                        | **1–20**       | Per job                                                |

Note the shapes of the gaps: there is no limit on _total_ queued requests, no
rate limit, and no cap on how long a queued print job waits. A caller who can
reach the port can keep the printer busy indefinitely. On a trusted LAN this is
a nuisance; anywhere else it is a denial of service.

While a job is running, `GET /status` returns `{"printing": true}` immediately
rather than blocking on the printer.

### The Printer Is Not a Trust Boundary

The LX-D02's own BLE "authentication" is **weak by design** — see
[docs/PROTOCOL.md](docs/PROTOCOL.md) for the handshake. Three things follow:

1. It authenticates the **client to the printer**, not the printer to the
   client. Nothing stops a device from impersonating a printer.
2. It provides **no confidentiality**. Print data crosses the air unencrypted at
   the application layer; anyone in BLE range with a sniffer can read what you
   print.
3. It is **not access control**. The handshake is reproducible from the
   published protocol — this project implements it, as do the three reference
   implementations credited in the README. **Anyone within Bluetooth range can
   print to the device regardless of what printa-ble does.**

Treat the printer as a device on an open channel. Do not print secrets on it,
and do not rely on the pairing as a security control. If you need physical
confidentiality, the answer is physical range, not the protocol.

### macOS Bluetooth Permission Is Per-Application

On macOS, Bluetooth access is gated by TCC and granted **per terminal
application**, not per binary. Granting permission to Terminal.app does not grant
it to iTerm2, VS Code's integrated terminal, or a launchd job — each one
triggers its own prompt the first time it scans, and a denied prompt looks like
"no printer found" rather than a permission error.

Manage these under System Settings → Privacy & Security → Bluetooth. Note the
converse: granting Bluetooth to your terminal grants it to **everything** you
run from that terminal, not just printa-ble.

## Deployment Guidance

**Do:**

- Run it on a **trusted LAN only** — the home or office network the printer
  already lives on.
- Use `--bind 127.0.0.1` if only the host machine needs to print, and reach it
  over an SSH tunnel from elsewhere.
- Pass `--no-remote-images` if you do not need remote images in markdown. It
  costs you a feature you probably are not using and removes the entire outbound
  fetch surface.
- Build with `--no-default-features` if you do not need URL printing. This also
  drops the Chrome dependency.
- Put a **reverse proxy in front of it with authentication and TLS** if it must
  be reachable beyond the local segment. Nginx or Caddy with basic auth is
  enough; the point is that printa-ble supplies none of this itself.
- Keep the printer and the server on a network segment you would be comfortable
  having someone print a page from.

**Do not:**

- Expose it to the public internet. There is no configuration that makes this
  safe.
- Run it on a hostile or shared network — coffee shop Wi-Fi, a conference LAN, a
  university network, a multi-tenant VLAN.
- Port-forward it, or hand it to a tunneling service like ngrok or Cloudflare
  Tunnel without authentication in front.
- Run it on a host with access to sensitive internal services you would not want
  scanned. The SSRF surface is real even when the printing surface is harmless.
- Treat "it's just a printer" as a reason to skip all of the above. The printing
  is the harmless part; the outbound requests are not.

### The Web App Is a Different Story

The static Web Bluetooth page in `web/` has no server and no backend. Rendering
runs client-side in WASM, and the browser mediates every fetch — remote images
are subject to CORS, and Bluetooth requires an explicit user gesture and a
secure context (`https` or `localhost`). It has none of the server's SSRF or
filesystem exposure, because it has no privileges the visitor's own browser
lacks.
