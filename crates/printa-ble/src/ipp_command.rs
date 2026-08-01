//! The `ippeveprinter -c` hook: turn an AirPrint job into a thermal print.
//!
//! `ippeveprinter` (shipped with macOS) is a complete IPP Everywhere server
//! with Bonjour advertising. It handles discovery, IPP and spooling, then runs
//! one command per job. That makes this module the whole of the AirPrint story
//! for now — see `docs/AIRPRINT.md`.
//!
//! Two contracts are load-bearing here, both established by watching a real
//! job rather than by reading the man page:
//!
//! - **the spooled job arrives as `argv[1]`**, not on stdin, and
//!   `CONTENT_TYPE` names its MIME type. A shim that forwards `"$@"` is
//!   therefore mandatory; one that does not makes every job abort with an
//!   empty read. Stdin is still accepted so the command stays pipeable by
//!   hand.
//! - **stderr is a control channel**, not a log. Lines prefixed `INFO:`,
//!   `ERROR:`, `STATE:` and `ATTR:` are parsed and surfaced in the client's
//!   print queue. Everything else is only visible under `-vv`.
//!
//! That second point is why this module prints its own progress instead of
//! leaving it to `tracing`: a thermal hold reported as `INFO:` shows up as
//! live status in the macOS print queue, where an ordinary log line would
//! look like the job had silently stalled.

use std::io::Read as _;
use std::path::Path;

use anyhow::{bail, Context as _};
use printa_ble_core::raster::{bitmap_to_png, decode_urf, pages_to_bitmap, Dither};

use crate::cli::IppCommandArgs;
use crate::print_service::{self, NoPaper, NoPrinterFound, PrintOptions, PrinterNotResponding};

/// Cap on the job we will buffer. The decoder bounds each page, but the raw
/// stream is attacker-influenced (any LAN host can queue a job), so bound it
/// before it reaches memory rather than after.
const MAX_JOB_BYTES: u64 = 64 << 20;

/// Leading bytes of a gzip member (RFC 1952).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Emit an `ippeveprinter` status line. No-op-safe when run by hand.
fn status(prefix: &str, msg: &str) {
    eprintln!("{prefix}: {msg}");
}

/// Read the job from `path`, or stdin when ippeveprinter invokes us.
fn read_job(path: Option<&Path>) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    match path {
        Some(p) => {
            let f = std::fs::File::open(p)
                .with_context(|| format!("cannot open job file {}", p.display()))?;
            std::io::BufReader::new(f)
                .take(MAX_JOB_BYTES + 1)
                .read_to_end(&mut buf)?;
        }
        None => {
            std::io::stdin()
                .lock()
                .take(MAX_JOB_BYTES + 1)
                .read_to_end(&mut buf)
                .context("cannot read the job from stdin")?;
        }
    }
    if buf.len() as u64 > MAX_JOB_BYTES {
        bail!("job larger than {} MiB", MAX_JOB_BYTES >> 20);
    }
    if buf.is_empty() {
        bail!("empty job (no document on stdin)");
    }
    Ok(buf)
}

/// Inflate a job the client chose to compress.
///
/// IPP lets a client compress the document (the `compression` operation
/// attribute), and iOS does: it sends Apple Raster as gzip. ippeveprinter
/// hands the job on exactly as received, so decompressing is the command's
/// job — without this, every print from an iPhone fails with a bad-magic
/// error, while the same document from a Mac works.
///
/// Detected by magic rather than by trusting `IPP_COMPRESSION`, so a client
/// that compresses without saying so still prints, and one that says so
/// without compressing does not get mangled.
fn maybe_inflate(bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    if !bytes.starts_with(&GZIP_MAGIC) {
        return Ok(bytes);
    }
    let mut out = Vec::new();
    // Bounded read: the compressed job was already capped, but gzip expands,
    // so cap the inflated size too rather than trusting the ratio.
    flate2::read::GzDecoder::new(&bytes[..])
        .take(MAX_JOB_BYTES + 1)
        .read_to_end(&mut out)
        .context("cannot inflate the compressed job")?;
    if out.len() as u64 > MAX_JOB_BYTES {
        bail!("job inflates to more than {} MiB", MAX_JOB_BYTES >> 20);
    }
    Ok(out)
}

/// `CONTENT_TYPE` is advisory: ippeveprinter also sends
/// `application/octet-stream` for a job whose format the client never named,
/// and the magic check in the decoder is the real gate. Only refuse a type we
/// positively know we cannot handle, so a mislabelled-but-valid job still
/// prints.
fn check_content_type() {
    match std::env::var("CONTENT_TYPE") {
        Ok(t) if t == "image/urf" || t == "application/octet-stream" => {}
        Ok(t) => status(
            "INFO",
            &format!("document declared {t}; trying to decode it as Apple Raster anyway"),
        ),
        Err(_) => {}
    }
}

pub async fn run(args: IppCommandArgs) -> anyhow::Result<i32> {
    check_content_type();
    let raw = read_job(args.file.as_deref())?;
    let compressed = raw.starts_with(&GZIP_MAGIC);
    let bytes = maybe_inflate(raw)?;
    if compressed {
        status(
            "INFO",
            &format!("inflated a gzipped job to {} bytes", bytes.len()),
        );
    }

    let pages = decode_urf(&bytes).context("cannot decode the job as Apple Raster (URF)")?;
    if pages.is_empty() {
        status("INFO", "job contained no pages; nothing to print");
        return Ok(0);
    }
    status("ATTR", &format!("job-impressions={}", pages.len()));
    let first = &pages[0];
    status(
        "INFO",
        &format!(
            "decoded {} page(s), {}x{} at {} dpi",
            pages.len(),
            first.width,
            first.height,
            first.dpi
        ),
    );

    let dither: Dither = args.dither.into();
    let bitmap = pages_to_bitmap(&pages, dither);
    if bitmap.height() == 0 {
        status("INFO", "every page was blank; nothing to print");
        return Ok(0);
    }

    if let Some(path) = args.preview.as_deref() {
        let png = bitmap_to_png(&bitmap);
        std::fs::write(path, png)
            .with_context(|| format!("cannot write preview to {}", path.display()))?;
        status(
            "INFO",
            &format!(
                "preview written to {} ({} lines)",
                path.display(),
                bitmap.height()
            ),
        );
        return Ok(0);
    }

    status("INFO", &format!("printing {} lines", bitmap.height()));
    let opts = PrintOptions {
        density: args.density,
        feed: args.feed,
        copies: 1,
    };

    match print_service::print_bitmap(bitmap, args.device.device.as_deref(), opts).await {
        Ok(outcome) => {
            // Clear any state this command set on a previous job, so a queue
            // does not stay stuck showing "out of paper" after a good print.
            status("STATE", "-media-empty,offline-report");
            status(
                "ATTR",
                &format!("job-impressions-completed={}", pages.len()),
            );
            let s = outcome.stats;
            // The counters that explain a slow job: thermal flow control is
            // normal on this hardware and looks like a stall without this.
            status(
                "INFO",
                &format!(
                    "printed {} lines in {:.1}s ({} holds, {} cooldowns, {} resends)",
                    outcome.lines,
                    outcome.elapsed.as_secs_f32(),
                    s.holds,
                    s.cooldowns,
                    s.retransmits
                ),
            );
            Ok(0)
        }
        Err(e) => {
            // Map our failure markers onto the IPP state keywords the print
            // queue knows how to display.
            if e.downcast_ref::<NoPaper>().is_some() {
                status("STATE", "media-empty");
            } else if e.downcast_ref::<NoPrinterFound>().is_some()
                || e.downcast_ref::<PrinterNotResponding>().is_some()
            {
                status("STATE", "offline-report");
            }
            status("ERROR", &format!("{e:#}"));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stdin_is_rejected_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.urf");
        std::fs::write(&path, b"").unwrap();
        let err = read_job(Some(&path)).unwrap_err();
        assert!(err.to_string().contains("empty job"), "{err}");
    }

    #[test]
    fn oversized_job_is_refused_before_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.urf");
        // One byte past the cap is enough; `take` stops there either way.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_JOB_BYTES + 1).unwrap();
        drop(f);
        let err = read_job(Some(&path)).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
    }

    #[test]
    fn a_real_job_file_round_trips_to_a_bitmap() {
        // The same capture the core decoder tests use, exercised through the
        // file-reading path this command actually takes.
        let bytes = include_bytes!("../../printa-ble-core/src/raster/testdata/letter_600dpi.urf");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job.urf");
        std::fs::write(&path, bytes).unwrap();

        let job = read_job(Some(&path)).unwrap();
        let pages = decode_urf(&job).unwrap();
        assert_eq!(pages.len(), 1);
        let bmp = pages_to_bitmap(&pages, Dither::FloydSteinberg);
        assert!(bmp.height() > 0);
    }

    /// The iPhone path: a gzipped job must decode exactly like a plain one.
    #[test]
    fn gzipped_job_inflates_and_decodes() {
        use std::io::Write as _;
        let plain: &[u8] =
            include_bytes!("../../printa-ble-core/src/raster/testdata/letter_600dpi.urf");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(plain).unwrap();
        let gz = enc.finish().unwrap();
        assert!(gz.starts_with(&GZIP_MAGIC));
        // Compressed bytes alone are not decodable — this is the failure iOS hit.
        assert!(decode_urf(&gz).is_err());

        let inflated = maybe_inflate(gz).unwrap();
        assert_eq!(inflated, plain);
        assert_eq!(decode_urf(&inflated).unwrap().len(), 1);
    }

    #[test]
    fn uncompressed_job_passes_through_untouched() {
        let plain = b"UNIRAST\0 not really".to_vec();
        assert_eq!(maybe_inflate(plain.clone()).unwrap(), plain);
    }

    #[test]
    fn non_urf_input_fails_instead_of_printing_noise() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job.pdf");
        std::fs::write(&path, b"%PDF-1.7\nnot a raster").unwrap();
        let job = read_job(Some(&path)).unwrap();
        assert!(decode_urf(&job).is_err());
    }
}
