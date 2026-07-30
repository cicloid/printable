//! Traditional Japanese pattern (和柄, *wagara*) separator bands.
//!
//! Five centuries-old motifs, drawn procedurally as a full-width decorative
//! band: `seigaiha` (青海波, blue sea waves), `asanoha` (麻の葉, hemp leaf),
//! `shippou` (七宝, seven treasures), `kikkou` (亀甲, tortoise shell) and
//! `ichimatsu` (市松, checkerboard). All are traditional and long out of
//! copyright; nothing here traces an existing drawing.
//!
//! # Why the geometry is built the way it is
//!
//! **Tiling.** A band is a separator, so it must run edge to edge with no
//! margin and no half-eaten motif at the paper's edge. Every pattern therefore
//! picks a horizontal period that divides 384 exactly ([`motifs_across`]), and
//! draws one motif past each edge. The rendered band is then genuinely periodic
//! — column `x` equals column `x + period` — which is what the tests assert.
//!
//! **Supersampling.** Arcs and diagonals thresholded straight to 1 bit look
//! ragged. Each pattern is drawn into a [`SS`]× oversampled coverage buffer and
//! collapsed by majority vote, so a stroke lands within a third of a pixel of
//! where the maths puts it. Strokes are [`STROKE`] supersampled units wide,
//! i.e. 2 px on paper: thin enough to read as a pattern, heavy enough to
//! survive a thermal head. [`ichimatsu`] needs none of this but goes through
//! the same buffer, so one code path produces every band.
//!
//! **Occlusion.** [`seigaiha`] is the only pattern whose motifs overlap. Real
//! seigaiha is layered like fish scales — a lower fan hides the bottom of the
//! one above — so each row erases its own half-discs before stroking its arcs,
//! painter's-algorithm style. Without that the arcs cross each other and the
//! pattern reads as noise.

use super::bitmap::{Bitmap, WIDTH};

/// Canonical pattern names, in the order the error message lists them.
pub const PATTERNS: [&str; 5] = ["asanoha", "ichimatsu", "kikkou", "seigaiha", "shippou"];

/// Smallest band height, in pixels.
pub const MIN_HEIGHT: u32 = 16;
/// Largest band height, in pixels.
pub const MAX_HEIGHT: u32 = 400;
/// Smallest motif scale.
pub const MIN_SCALE: u32 = 1;
/// Largest motif scale.
pub const MAX_SCALE: u32 = 4;
/// Default band height, in pixels.
pub const DEFAULT_HEIGHT: u32 = 56;

/// Errors from wagara rendering.
#[derive(Debug, thiserror::Error)]
pub enum WagaraError {
    /// The fence named a pattern this module does not draw.
    #[error("unknown wagara pattern {0:?} (valid: asanoha, ichimatsu, kikkou, seigaiha, shippou)")]
    UnknownPattern(String),
    /// A `key: value` line in the fence body was malformed or out of range.
    #[error("{0}")]
    BadOption(String),
}

/// Tuning knobs from a wagara fence body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WagaraOptions {
    /// Band height in pixels.
    pub height: u32,
    /// Motif size multiplier.
    pub scale: u32,
}

impl Default for WagaraOptions {
    fn default() -> Self {
        Self {
            height: DEFAULT_HEIGHT,
            scale: MIN_SCALE,
        }
    }
}

/// Supersampling factor: each printed pixel is drawn as an `SS`×`SS` block and
/// collapsed by majority vote.
const SS: usize = 3;
/// Width of the oversampled coverage buffer, in supersampled units.
const SS_WIDTH: usize = WIDTH * SS;
/// Stroke half-width in supersampled units — 2 * 3 = 6 units, i.e. 2 px.
const STROKE: f64 = 3.0;

/// Resolve a spelling to its canonical pattern name.
///
/// Matching is case-insensitive and tolerates the two romanisations that differ
/// only in a long vowel: `shippo`/`shippou` and `kikko`/`kikkou` are the same
/// motif, and a reader who typed either meant the same band.
fn canonical(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "asanoha" => Some("asanoha"),
        "ichimatsu" => Some("ichimatsu"),
        "kikkou" | "kikko" => Some("kikkou"),
        "seigaiha" => Some("seigaiha"),
        "shippou" | "shippo" => Some("shippou"),
        _ => None,
    }
}

/// Render `pattern` as a full-width decorative band.
///
/// Out-of-range options are clamped rather than rejected: [`WagaraOptions`] is
/// public, so a caller can build one [`parse_wagara_options`] would have turned
/// down, and a separator is not worth a panic. The fence path validates first,
/// so a reader still gets a diagnostic for a bad `height:` line.
pub fn render_wagara(pattern: &str, opts: WagaraOptions) -> Result<Bitmap, WagaraError> {
    let name = canonical(pattern)
        .ok_or_else(|| WagaraError::UnknownPattern(pattern.trim().to_string()))?;
    let height = opts.height.clamp(MIN_HEIGHT, MAX_HEIGHT) as usize;
    let scale = opts.scale.clamp(MIN_SCALE, MAX_SCALE) as f64;

    let mut canvas = Canvas::new(height);
    match name {
        "asanoha" => asanoha(&mut canvas, scale),
        "ichimatsu" => ichimatsu(&mut canvas, scale),
        "kikkou" => kikkou(&mut canvas, scale),
        "seigaiha" => seigaiha(&mut canvas, scale),
        "shippou" => shippou(&mut canvas, scale),
        other => unreachable!("canonical() returned an undrawn pattern {other:?}"),
    }
    Ok(canvas.into_bitmap())
}

/// Parse a wagara fence body: zero or more `key: value` lines.
///
/// Blank lines are ignored, keys are case-insensitive, and spacing is free.
/// Anything else is an error rather than a silent default — a separator that
/// quietly ignored `heigth: 80` would just look wrong with no way to tell why.
pub fn parse_wagara_options(body: &str) -> Result<WagaraOptions, WagaraError> {
    let mut opts = WagaraOptions::default();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(WagaraError::BadOption(format!(
                "wagara option {line:?} is not a `key: value` line (valid keys: height, scale)"
            )));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let (target, lo, hi) = match key.as_str() {
            "height" => (&mut opts.height, MIN_HEIGHT, MAX_HEIGHT),
            "scale" => (&mut opts.scale, MIN_SCALE, MAX_SCALE),
            _ => {
                return Err(WagaraError::BadOption(format!(
                    "unknown wagara option {key:?} (valid: height, scale)"
                )))
            }
        };
        let parsed: u32 = value.parse().map_err(|_| {
            WagaraError::BadOption(format!(
                "wagara {key} must be a whole number, got {value:?}"
            ))
        })?;
        if !(lo..=hi).contains(&parsed) {
            return Err(WagaraError::BadOption(format!(
                "wagara {key} must be between {lo} and {hi}, got {parsed}"
            )));
        }
        *target = parsed;
    }
    Ok(opts)
}

/// How many motifs fit across the roll, given a desired motif width in pixels.
///
/// The answer is always a divisor of 384 (and an even one, so a lattice may be
/// offset by half a period and stay periodic), which is what makes a band tile:
/// the pattern's period is `384 / n` px exactly, so the motif at the right edge
/// continues into the one at the left. The desired width is only a target —
/// `scale` nudges it, and the nearest usable count wins.
fn motifs_across(desired_px: f64) -> usize {
    // Divisors of 192; each yields a period 384/n whose supersampled form is an
    // even number of units, so half-period row offsets stay exact.
    const COUNTS: [usize; 14] = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 192];
    let error = |n: usize| (WIDTH as f64 / n as f64 - desired_px).abs();
    COUNTS
        .into_iter()
        .min_by(|&a, &b| error(a).total_cmp(&error(b)))
        .expect("COUNTS is not empty")
}

/// An oversampled 1-bit drawing surface, `SS`× the printed resolution.
///
/// All coordinates are in supersampled units with the origin at the band's
/// top-left corner, so a motif's geometry is written once at full precision and
/// the majority vote in [`Canvas::into_bitmap`] does the antialiasing.
struct Canvas {
    /// Height in supersampled units.
    height: usize,
    ink: Vec<bool>,
}

impl Canvas {
    fn new(height_px: usize) -> Self {
        let height = height_px * SS;
        Self {
            height,
            ink: vec![false; SS_WIDTH * height],
        }
    }

    /// Set every sample inside `shape` to `value`, within the bounding box
    /// `(x0, y0)..(x1, y1)`.
    ///
    /// The box is only an optimisation — a wrong one costs correctness, so
    /// callers pad it by the stroke half-width. Samples are taken at each
    /// supersample's centre.
    fn paint(
        &mut self,
        (x0, y0, x1, y1): (f64, f64, f64, f64),
        value: bool,
        shape: impl Fn(f64, f64) -> bool,
    ) {
        let xs = clamp_span(x0, x1, SS_WIDTH);
        let ys = clamp_span(y0, y1, self.height);
        for sy in ys {
            let py = sy as f64 + 0.5;
            for sx in xs.clone() {
                if shape(sx as f64 + 0.5, py) {
                    self.ink[sy * SS_WIDTH + sx] = value;
                }
            }
        }
    }

    /// Stroke a circle of radius `r`, keeping only samples where `keep` holds
    /// (used to cut a full circle down to the upper half).
    fn stroke_arc(&mut self, cx: f64, cy: f64, r: f64, keep: impl Fn(f64, f64) -> bool) {
        let reach = r + STROKE;
        self.paint(
            (cx - reach, cy - reach, cx + reach, cy + reach),
            true,
            |x, y| {
                let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                (d - r).abs() <= STROKE && keep(x, y)
            },
        );
    }

    fn stroke_circle(&mut self, cx: f64, cy: f64, r: f64) {
        self.stroke_arc(cx, cy, r, |_, _| true);
    }

    /// Fill (or clear) the upper half of a disc — seigaiha's occluder.
    fn upper_disc(&mut self, cx: f64, cy: f64, r: f64, value: bool) {
        self.paint((cx - r, cy - r, cx + r, cy), value, |x, y| {
            y <= cy && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
        });
    }

    /// Stroke the segment from `a` to `b` with butt-free round joins: a sample
    /// is inked when it lies within [`STROKE`] of the segment.
    fn stroke_segment(&mut self, (ax, ay): (f64, f64), (bx, by): (f64, f64)) {
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        self.paint(
            (
                ax.min(bx) - STROKE,
                ay.min(by) - STROKE,
                ax.max(bx) + STROKE,
                ay.max(by) + STROKE,
            ),
            true,
            |x, y| {
                // Projection parameter of (x, y) onto the segment, clamped to
                // its ends so the caps are round rather than infinite.
                let t = if len2 == 0.0 {
                    0.0
                } else {
                    (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0)
                };
                let (px, py) = (ax + t * dx, ay + t * dy);
                (x - px).powi(2) + (y - py).powi(2) <= STROKE * STROKE
            },
        );
    }

    fn fill_rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64) {
        self.paint((x0, y0, x1, y1), true, |x, y| {
            x >= x0 && x < x1 && y >= y0 && y < y1
        });
    }

    /// Collapse the oversampled buffer to 1 bit: a printed pixel is black when
    /// at least half its samples are inked.
    fn into_bitmap(self) -> Bitmap {
        let height_px = self.height / SS;
        let mut out = Bitmap::new(height_px);
        for y in 0..height_px {
            for x in 0..WIDTH {
                let covered: usize = (0..SS)
                    .map(|dy| {
                        let row = (y * SS + dy) * SS_WIDTH + x * SS;
                        (0..SS).filter(|&dx| self.ink[row + dx]).count()
                    })
                    .sum();
                // covered / (SS * SS) >= 0.5, without the division.
                if 2 * covered >= SS * SS {
                    out.set(x, y, true);
                }
            }
        }
        out
    }
}

/// Clamp a floating-point span to the sample indices it touches.
fn clamp_span(a: f64, b: f64, limit: usize) -> std::ops::Range<usize> {
    let lo = a.floor().clamp(0.0, limit as f64) as usize;
    let hi = b.ceil().clamp(0.0, limit as f64) as usize;
    lo..hi.max(lo)
}

/// Lattice rows covering `[-overhang, height + overhang]` at pitch `step`,
/// anchored so a row sits at the band's vertical centre.
///
/// Anchoring on the centre is what "visually centred" means for a periodic
/// band: whatever the height, the middle of the paper falls on a motif rather
/// than wherever the lattice happened to start.
fn rows(canvas: &Canvas, step: f64, overhang: f64) -> impl Iterator<Item = (i64, f64)> + '_ {
    let centre = canvas.height as f64 / 2.0;
    let first = ((-overhang - centre) / step).floor() as i64;
    let last = ((canvas.height as f64 + overhang - centre) / step).ceil() as i64;
    (first..=last).map(move |k| (k, centre + k as f64 * step))
}

/// 青海波 — concentric half-circle fans laid like fish scales.
///
/// Fans of radius `r` sit at pitch `2r` along a row, so neighbours meet exactly
/// (no overlap, hence no arbitrary front-to-back order within a row); rows are
/// half a pitch apart horizontally and `r / 2` apart vertically. That vertical
/// pitch is the tightest that still leaves no gap between rows and the loosest
/// that keeps every fan's apex visible all the way to its centre — which is
/// what gives seigaiha its stack of nested arcs.
fn seigaiha(canvas: &mut Canvas, scale: f64) {
    const ARCS: usize = 3;
    let n = motifs_across(48.0 * scale);
    let pitch = (SS_WIDTH / n) as f64;
    let r = pitch / 2.0;
    let step = r / 2.0;

    let plan: Vec<(f64, f64)> = rows(canvas, step, r)
        .flat_map(|(k, y)| {
            let offset = if k.rem_euclid(2) == 1 { r } else { 0.0 };
            (-1..=n as i64 + 1).map(move |i| (i as f64 * pitch + offset, y))
        })
        .collect();
    // Group by row so a whole row occludes what is behind it before any of its
    // own arcs are drawn — otherwise a fan would erase its neighbour's stroke.
    for row in plan.chunks(n + 3) {
        for &(x, y) in row {
            canvas.upper_disc(x, y, r, false);
        }
        for &(x, y) in row {
            for a in 1..=ARCS {
                canvas.stroke_arc(x, y, r * a as f64 / ARCS as f64, |_, py| py <= y);
            }
        }
    }
}

/// 麻の葉 — the hemp leaf: a hexagonal lattice with all three long diagonals,
/// and a three-armed spur to the centroid of each of the six triangles those
/// diagonals cut.
///
/// The spurs are not decoration, they are the pattern. Hexagon edges plus
/// diagonals alone come out to a plain triangular grid — the hexagon's edge and
/// its circumradius are the same length, so the centres and the corners become
/// indistinguishable and nothing reads as a leaf. Subdividing each triangle at
/// its centroid breaks that symmetry and produces the twelve-fold star the
/// motif is known for.
///
/// Pointy-top hexagons: width `√3 a`, row pitch `1.5 a`, alternate rows offset
/// half a width. The horizontal period is one hexagon width, because shifting
/// by it maps every row's lattice onto itself.
fn asanoha(canvas: &mut Canvas, scale: f64) {
    let n = motifs_across(64.0 * scale);
    let pitch = (SS_WIDTH / n) as f64;
    let a = pitch / 3f64.sqrt();
    for (k, y) in rows(canvas, 1.5 * a, a).collect::<Vec<_>>() {
        let offset = if k.rem_euclid(2) == 1 {
            pitch / 2.0
        } else {
            0.0
        };
        for i in -1..=n as i64 + 1 {
            let centre = (i as f64 * pitch + offset, y);
            let v = hexagon(centre, a, -90.0);
            for e in 0..6 {
                canvas.stroke_segment(v[e], v[(e + 1) % 6]);
            }
            for d in 0..3 {
                canvas.stroke_segment(v[d], v[d + 3]);
            }
            for t in 0..6 {
                let (p, q) = (v[t], v[(t + 1) % 6]);
                let g = ((centre.0 + p.0 + q.0) / 3.0, (centre.1 + p.1 + q.1) / 3.0);
                for corner in [centre, p, q] {
                    canvas.stroke_segment(g, corner);
                }
            }
        }
    }
}

/// 亀甲 — a plain hexagonal honeycomb outline.
///
/// Flat-top hexagons (a horizontal edge top and bottom, points left and right),
/// which is how the tortoise-shell motif is drawn on textiles and crests.
/// Alternate *columns* are offset vertically, so the horizontal period is two
/// column pitches — `3a`, the value fitted to the roll.
fn kikkou(canvas: &mut Canvas, scale: f64) {
    let n = motifs_across(48.0 * scale);
    let period = (SS_WIDTH / n) as f64;
    let a = period / 3.0;
    let column = period / 2.0;
    let tall = 3f64.sqrt() * a;
    for (_, y) in rows(canvas, tall, tall).collect::<Vec<_>>() {
        for i in -1..=2 * n as i64 + 1 {
            let stagger = if i.rem_euclid(2) == 1 {
                tall / 2.0
            } else {
                0.0
            };
            let v = hexagon((i as f64 * column, y + stagger), a, 0.0);
            for e in 0..6 {
                canvas.stroke_segment(v[e], v[(e + 1) % 6]);
            }
        }
    }
}

/// 七宝 — equal circles on a square lattice, overlapping into four-petal
/// flowers.
///
/// The radius is `pitch / √2`, which is what makes the figure close: each
/// circle then passes through the centre of all four cells it touches, so two
/// neighbouring circles meet exactly at those two points and the lens between
/// them is a full petal, tip to tip. Four petals therefore ring every lattice
/// point and four petal tips meet at every cell centre — the interlocking
/// "seven treasures" figure.
///
/// A radius equal to the pitch (each circle through its neighbours' *centres*)
/// also tiles, but every circle then overlaps its diagonal neighbours too and
/// the petals disappear into a mesh.
fn shippou(canvas: &mut Canvas, scale: f64) {
    let n = motifs_across(32.0 * scale);
    let pitch = (SS_WIDTH / n) as f64;
    for (_, y) in rows(canvas, pitch, pitch).collect::<Vec<_>>() {
        for i in -1..=n as i64 + 1 {
            canvas.stroke_circle(i as f64 * pitch, y, pitch / 2f64.sqrt());
        }
    }
}

/// 市松 — a checkerboard of solid blocks.
///
/// The column pitch is fixed by the roll, as everywhere else here. The row
/// pitch is not: it is the band height divided by the whole number of rows that
/// comes closest to square, so the band always ends on a row boundary. Holding
/// the cells exactly square instead would leave a clipped sliver of a row along
/// the top and bottom edges of most heights, which reads as a rendering fault
/// rather than a pattern — an off-square block does not.
fn ichimatsu(canvas: &mut Canvas, scale: f64) {
    let n = motifs_across(48.0 * scale);
    let period = (SS_WIDTH / n) as f64;
    let cell = period / 2.0;
    let count = (canvas.height as f64 / cell).round().max(1.0);
    let row_height = canvas.height as f64 / count;
    for j in 0..count as i64 {
        let y = j as f64 * row_height;
        for i in 0..2 * n as i64 {
            if (i + j).rem_euclid(2) == 0 {
                let x = i as f64 * cell;
                canvas.fill_rect(x, y, x + cell, y + row_height);
            }
        }
    }
}

/// The six vertices of a regular hexagon of circumradius `a`, the first at
/// `first_deg` measured clockwise from the +x axis (screen y grows downward).
fn hexagon((cx, cy): (f64, f64), a: f64, first_deg: f64) -> [(f64, f64); 6] {
    std::array::from_fn(|k| {
        let theta = (first_deg + 60.0 * k as f64).to_radians();
        (cx + a * theta.cos(), cy + a * theta.sin())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::bitmap::WIDTH;

    /// Every divisor of 384 below the full width — the periods a band may
    /// legitimately repeat at.
    fn divisors() -> Vec<usize> {
        (1..WIDTH).filter(|d| WIDTH.is_multiple_of(*d)).collect()
    }

    fn column(b: &Bitmap, x: usize) -> Vec<bool> {
        (0..b.height()).map(|y| b.get(x, y)).collect()
    }

    fn ink(b: &Bitmap) -> usize {
        (0..b.height())
            .map(|y| (0..WIDTH).filter(|&x| b.get(x, y)).count())
            .sum()
    }

    /// Ink anywhere in the columns `xs`.
    fn ink_in_columns(b: &Bitmap, xs: std::ops::Range<usize>) -> bool {
        xs.into_iter().any(|x| (0..b.height()).any(|y| b.get(x, y)))
    }

    /// Smallest period `p` (a divisor of 384) with `column(x) == column(x + p)`
    /// for every `x`, or `None` if the band does not repeat within its width.
    fn horizontal_period(b: &Bitmap) -> Option<usize> {
        divisors()
            .into_iter()
            .find(|&p| (0..WIDTH).all(|x| column(b, x) == column(b, (x + p) % WIDTH)))
    }

    fn render(name: &str) -> Bitmap {
        render_wagara(name, WagaraOptions::default())
            .unwrap_or_else(|e| panic!("{name} should render: {e}"))
    }

    #[test]
    fn every_pattern_renders_a_full_width_band() {
        for name in PATTERNS {
            let b = render(name);
            assert_eq!(
                b.height(),
                DEFAULT_HEIGHT as usize,
                "{name} band should be {DEFAULT_HEIGHT} px tall"
            );
            let ink = ink(&b);
            let total = WIDTH * b.height();
            assert!(ink > 0, "{name} band has no ink");
            // A separator is a texture, not a blackout: somewhere between
            // "visible" and "a solid bar" for every pattern.
            assert!(
                ink * 20 > total,
                "{name} covers only {ink}/{total} px — too faint to read"
            );
            assert!(
                ink * 10 < total * 8,
                "{name} covers {ink}/{total} px — nearly solid black"
            );
        }
    }

    #[test]
    fn every_pattern_reaches_both_edges() {
        for name in PATTERNS {
            let b = render(name);
            assert!(
                ink_in_columns(&b, 0..8),
                "{name} leaves a blank left margin — the band is not edge-to-edge"
            );
            assert!(
                ink_in_columns(&b, WIDTH - 8..WIDTH),
                "{name} leaves a blank right margin — the band is not edge-to-edge"
            );
        }
    }

    /// The band must wrap: a pattern whose columns repeat at a divisor of the
    /// roll width continues across the paper edge with no seam and no clipped
    /// motif.
    #[test]
    fn every_pattern_tiles_horizontally() {
        for name in PATTERNS {
            let b = render(name);
            let period = horizontal_period(&b)
                .unwrap_or_else(|| panic!("{name} does not repeat within the 384 px width"));
            assert!(
                period <= WIDTH / 2,
                "{name} repeats only every {period} px — barely a pattern"
            );
        }
    }

    #[test]
    fn height_option_is_respected() {
        for name in PATTERNS {
            for height in [MIN_HEIGHT, 32, 120, MAX_HEIGHT] {
                let b = render_wagara(
                    name,
                    WagaraOptions {
                        height,
                        ..Default::default()
                    },
                )
                .expect("valid pattern");
                assert_eq!(b.height(), height as usize, "{name} at height {height}");
                assert!(ink(&b) > 0, "{name} at height {height} has no ink");
            }
        }
    }

    #[test]
    fn scale_enlarges_the_motif() {
        for name in PATTERNS {
            let small = render_wagara(name, WagaraOptions::default()).expect("valid");
            let big = render_wagara(
                name,
                WagaraOptions {
                    scale: 2,
                    ..Default::default()
                },
            )
            .expect("valid");
            let (sp, bp) = (
                horizontal_period(&small).expect("scale 1 tiles"),
                horizontal_period(&big).expect("scale 2 tiles"),
            );
            assert!(
                bp > sp,
                "{name} at scale 2 repeats every {bp} px, no coarser than scale 1's {sp}"
            );
        }
    }

    #[test]
    fn out_of_range_options_are_clamped_not_panicking() {
        // `WagaraOptions` is public, so a caller can build one the parser would
        // have rejected. Rendering clamps rather than panicking.
        let b = render_wagara(
            "seigaiha",
            WagaraOptions {
                height: 0,
                scale: 99,
            },
        )
        .expect("valid pattern");
        assert_eq!(b.height(), MIN_HEIGHT as usize);
    }

    #[test]
    fn pattern_names_are_case_insensitive_and_aliased() {
        let rows = |b: &Bitmap| (0..b.height()).map(|y| *b.row(y)).collect::<Vec<_>>();
        for (alias, canonical) in [
            ("SEIGAIHA", "seigaiha"),
            ("  Asanoha ", "asanoha"),
            ("shippo", "shippou"),
            ("Kikko", "kikkou"),
            ("ICHIMATSU", "ichimatsu"),
        ] {
            let a = render(alias);
            let c = render(canonical);
            assert_eq!(rows(&a), rows(&c), "{alias} should render as {canonical}");
        }
    }

    #[test]
    fn unknown_pattern_lists_the_valid_names() {
        let err = render_wagara("nonsense", WagaraOptions::default())
            .expect_err("nonsense is not a pattern");
        let message = err.to_string();
        assert!(
            message.contains("nonsense"),
            "message should name the offender: {message}"
        );
        for name in PATTERNS {
            assert!(
                message.contains(name),
                "message should list {name}: {message}"
            );
        }
        assert!(matches!(err, WagaraError::UnknownPattern(_)));
    }

    #[test]
    fn rendering_is_deterministic() {
        for name in PATTERNS {
            let a = render(name);
            let b = render(name);
            for y in 0..a.height() {
                assert_eq!(a.row(y), b.row(y), "{name} row {y} differs between renders");
            }
        }
    }

    #[test]
    fn options_default_when_the_body_is_empty() {
        for body in ["", "\n", "   \n\t\n"] {
            assert_eq!(
                parse_wagara_options(body).expect("blank body is valid"),
                WagaraOptions::default(),
                "{body:?}"
            );
        }
    }

    #[test]
    fn options_parse_height_and_scale() {
        let opts = parse_wagara_options("height: 60\nscale: 2").expect("valid options");
        assert_eq!(
            opts,
            WagaraOptions {
                height: 60,
                scale: 2
            }
        );
        // Keys are case-insensitive and the spacing is free-form.
        let opts = parse_wagara_options("  HEIGHT :100  \n\nScale:4\n").expect("valid options");
        assert_eq!(
            opts,
            WagaraOptions {
                height: 100,
                scale: 4
            }
        );
    }

    #[test]
    fn options_reject_unknown_keys() {
        let err = parse_wagara_options("colour: red").expect_err("colour is not an option");
        assert!(matches!(err, WagaraError::BadOption(_)));
        let message = err.to_string();
        assert!(message.contains("colour"), "{message}");
        assert!(message.contains("height"), "{message}");
        assert!(message.contains("scale"), "{message}");
    }

    #[test]
    fn options_reject_lines_without_a_colon() {
        let err = parse_wagara_options("height 60").expect_err("no colon");
        assert!(matches!(err, WagaraError::BadOption(_)));
        assert!(err.to_string().contains("height 60"), "{err}");
    }

    #[test]
    fn options_reject_non_numeric_values() {
        for body in ["height: tall", "scale: x2", "height: 6.5", "scale: -1"] {
            let err = parse_wagara_options(body)
                .err()
                .unwrap_or_else(|| panic!("{body:?} should be rejected"));
            assert!(matches!(err, WagaraError::BadOption(_)), "{body:?}");
            assert!(err.to_string().contains("whole number"), "{body:?} → {err}");
        }
    }

    #[test]
    fn options_reject_out_of_range_values() {
        for body in [
            format!("height: {}", MIN_HEIGHT - 1),
            format!("height: {}", MAX_HEIGHT + 1),
            "height: 0".to_string(),
            format!("scale: {}", MAX_SCALE + 1),
            "scale: 0".to_string(),
        ] {
            let err = parse_wagara_options(&body)
                .err()
                .unwrap_or_else(|| panic!("{body:?} should be rejected"));
            assert!(matches!(err, WagaraError::BadOption(_)), "{body:?}");
        }
    }
}
