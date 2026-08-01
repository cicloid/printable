//! Raster image handling: 1-bit bitmaps and raster packet chunking.

pub mod barcode;
pub mod bitmap;
pub mod dither;
pub mod markdown;
pub mod preview;
pub mod qr;
pub mod rich;
pub mod text;
pub mod urf;
pub mod wagara;

pub use barcode::{render_barcode, BarcodeError};
pub use bitmap::Bitmap;
pub use dither::{image_to_bitmap, prepare, Dither};
pub use markdown::{markdown_image_refs, render_markdown, render_markdown_with};
pub use preview::bitmap_to_png;
pub use qr::{render_qr, QrError};
pub use rich::{render_rich, FontStyle, RichLine, Span, Style};
pub use text::render_text;
pub use urf::{decode_urf, pages_to_bitmap, UrfError, UrfPage};
pub use wagara::{parse_wagara_options, render_wagara, WagaraError, WagaraOptions};
