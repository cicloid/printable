//! End-to-end coverage of the `print` input paths that read stdin.
//!
//! Everything here goes through `--preview`, so no printer is involved: the
//! binary is driven exactly as a user would drive it and the resulting PNG is
//! measured.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Headings are set in larger faces than body text, so a document that is
/// mostly headings is unmistakably taller rendered as markdown than as the
/// literal source. Height is the signal every assertion below reads.
const DOC: &str = "# Title\n## Section\n### Detail\n";

/// Run `printable print …`, feeding `stdin`, and return the raw output.
fn run_print(args: &[&str], stdin: &str) -> Output {
    run_print_in(std::env::current_dir().unwrap(), args, stdin)
}

/// The same, from a chosen working directory.
fn run_print_in(cwd: impl AsRef<Path>, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_printable"))
        .current_dir(cwd)
        .arg("print")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn printable");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("failed to write stdin");
    child.wait_with_output().expect("failed to wait")
}

/// Render `stdin` to a preview PNG in `dir` and return that PNG's pixel height.
fn preview_height(dir: &Path, name: &str, args: &[&str], stdin: &str) -> u32 {
    let png = dir.join(name);
    let mut argv = vec!["--preview", png.to_str().unwrap()];
    argv.extend_from_slice(args);
    let out = run_print(&argv, stdin);
    assert!(
        out.status.success(),
        "printable failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    png_height(&std::fs::read(&png).expect("preview PNG was not written"))
}

/// Height from a PNG's IHDR chunk, which always starts at a fixed offset.
fn png_height(bytes: &[u8]) -> u32 {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    u32::from_be_bytes(bytes[20..24].try_into().unwrap())
}

/// The reported bug: piped markdown used to come out as literal source.
#[test]
fn piped_markdown_renders_taller_than_the_same_bytes_as_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let md = preview_height(dir.path(), "md.png", &["-m"], DOC);
    let plain = preview_height(dir.path(), "plain.png", &[], DOC);
    assert!(md > plain, "markdown {md}px, plain {plain}px");
}

/// The same flag on a positional argument, not just on stdin.
#[test]
fn markdown_flag_applies_to_a_text_argument() {
    let dir = tempfile::tempdir().unwrap();
    let md = preview_height(dir.path(), "arg-md.png", &["-m", DOC], "");
    let plain = preview_height(dir.path(), "arg-plain.png", &[DOC], "");
    assert!(md > plain, "markdown {md}px, plain {plain}px");
}

/// `-f -` is the Unix spelling of "read stdin", and composes with `-m`.
#[test]
fn dash_file_reads_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let plain = preview_height(dir.path(), "dash.png", &["-f", "-"], DOC);
    let piped = preview_height(dir.path(), "pipe.png", &[], DOC);
    assert_eq!(plain, piped, "`-f -` must match a bare pipe");

    let md = preview_height(dir.path(), "dash-md.png", &["-f", "-", "-m"], DOC);
    assert!(md > plain, "markdown {md}px, plain {plain}px");
}

/// Redundant, not wrong: a `.md` file already renders as markdown.
#[test]
fn markdown_flag_is_silently_accepted_for_a_markdown_file() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("notes.md");
    std::fs::write(&doc, DOC).unwrap();
    let path = doc.to_str().unwrap();

    let with_flag = preview_height(dir.path(), "f-md.png", &["-f", path, "-m"], "");
    let without = preview_height(dir.path(), "f-plain.png", &["-f", path], "");
    assert_eq!(with_flag, without);
}

/// A bitmap is not a document; the error must name the flag that is wrong.
#[test]
fn markdown_flag_with_an_image_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("photo.png");
    let out = run_print(&["-f", png.to_str().unwrap(), "-m"], "");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--markdown"), "stderr: {stderr}");
}

/// A rendered web page is not a document either; clap catches this one.
#[cfg(feature = "url")]
#[test]
fn markdown_flag_with_url_is_rejected_by_the_parser() {
    let out = run_print(&["-m", "--url", "http://127.0.0.1:1/"], "");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected a conflict error, got: {stderr}"
    );
}

/// Piped markdown has no source file, so `![](logo.png)` must mean "the
/// logo.png I am standing in", not a placeholder.
#[test]
fn piped_markdown_resolves_images_against_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("logo.png"), tall_png()).unwrap();

    let out = run_print_in(
        dir.path(),
        &["-m", "--preview", "out.png"],
        "![logo](logo.png)",
    );
    assert!(
        out.status.success(),
        "printable failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let resolved = png_height(&std::fs::read(dir.path().join("out.png")).unwrap());

    // Same document, a directory with no logo.png: an italic placeholder.
    let out = run_print_in(
        empty.path(),
        &["-m", "--preview", "out.png"],
        "![logo](logo.png)",
    );
    assert!(out.status.success());
    let placeholder = png_height(&std::fs::read(empty.path().join("out.png")).unwrap());

    assert!(
        resolved > placeholder,
        "resolved {resolved}px, placeholder {placeholder}px"
    );
}

/// A 384x200 all-white PNG — tall enough that resolving it is unmistakable.
fn tall_png() -> Vec<u8> {
    let mut png = Vec::new();
    let img = image::GrayImage::new(384, 200);
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// Empty stdin is still nothing to print, `-m` or not.
#[test]
fn empty_stdin_is_refused_with_the_markdown_flag() {
    let out = run_print(&["-m"], "\n\n  \n");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing to print"), "stderr: {stderr}");
}
