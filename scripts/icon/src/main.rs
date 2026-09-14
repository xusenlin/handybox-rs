//! Derive the application icons from the untouched logo.
//! Run with `task icon`; the generated PNGs are committed so an ordinary
//! `cargo build` never depends on this tool.
//!
//! Two derivatives, because the platforms disagree about margins, and both are
//! kept small because they are embedded in the executable:
//!
//! * `app-icon.png` fills its canvas. The sidebar brand and the Slint window
//!   icon size their own box, so any built-in margin would just shrink the mark.
//! * `macos-icon.png` follows Apple's icon grid, where the artwork covers
//!   824 of 1024 points and the rest is transparent. Without that margin the
//!   Dock tile renders noticeably larger than every neighbouring application.
use image::{imageops, GenericImageView, Rgba, RgbaImage};

/// Apple's icon grid: artwork width over canvas width.
const BODY: f64 = 824.0 / 1024.0;
/// Corner radius over artwork width, also from the icon grid (185.4 / 824).
const RADIUS: f64 = 185.4 / 824.0;

/// Round the corners of `body` and centre it on a transparent `canvas` square.
fn rounded(body: &RgbaImage, canvas_size: u32) -> RgbaImage {
    let (width, height) = body.dimensions();
    let radius = width.min(height) as f64 * RADIUS;
    let (right, bottom) = (width as f64, height as f64);
    let mut canvas = RgbaImage::new(canvas_size, canvas_size);

    for (x, y, pixel) in body.enumerate_pixels() {
        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
        // Distance outside the rounded rectangle: zero everywhere except within
        // the four corner squares, where the circular arc takes over.
        let dx = (radius - px).max(px - (right - radius)).max(0.0);
        let dy = (radius - py).max(py - (bottom - radius)).max(0.0);
        // Antialias across one pixel rather than cutting hard.
        let coverage = (radius + 0.5 - dx.hypot(dy)).clamp(0.0, 1.0);
        let [r, g, b, a] = pixel.0;
        let alpha = (a as f64 * coverage).round() as u8;
        canvas.put_pixel(
            x + (canvas_size - width) / 2,
            y + (canvas_size - height) / 2,
            Rgba([r, g, b, alpha]),
        );
    }
    canvas
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = image::open(root.join("assets/logo.png"))?;
    let (width, height) = source.dimensions();
    assert_eq!(width, height, "the logo must be square");

    // Canvas sizes are deliberately modest: 256 covers a 46pt sidebar mark and
    // the window icon on a 2x display, and 512 covers the largest Dock tile.
    // Every doubling roughly quadruples the bytes compiled into the binary.
    for (name, canvas, inset) in [("app-icon.png", 256, false), ("macos-icon.png", 512, true)] {
        let artwork = if inset {
            (canvas as f64 * BODY).round() as u32
        } else {
            canvas
        };
        let body = imageops::resize(&source, artwork, artwork, imageops::FilterType::Lanczos3);
        let destination = root.join("assets").join(name);
        rounded(&body, canvas).save(&destination)?;
        let bytes = std::fs::metadata(&destination)?.len();
        println!(
            "Built {} ({artwork}px artwork on {canvas}px canvas, {} KB)",
            destination.display(),
            bytes / 1024
        );
    }
    Ok(())
}
