//! Raster image handling: 1-bit bitmaps and raster packet chunking.

pub mod bitmap;
pub mod dither;
pub mod preview;
pub mod rich;
pub mod text;

pub use bitmap::Bitmap;
pub use dither::{image_to_bitmap, prepare, Dither};
pub use preview::bitmap_to_png;
pub use rich::{render_rich, FontStyle, RichLine, Span, Style};
pub use text::render_text;
