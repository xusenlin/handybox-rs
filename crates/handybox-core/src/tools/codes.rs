//! Local recognition of QR codes and barcodes in an image.
//!
//! One pass produces everything the page needs: the symbols themselves, where
//! each one sits in the picture, and a thumbnail to draw them over. The image is
//! decoded once, here, because the recognizer wants grayscale and the preview
//! wants pixels, and reading the file twice to get both would be the only other
//! way.
//!
//! Positions come out normalized to the image, not in pixels. What is on screen
//! is a scaled-down preview, so a fraction of the picture is the only coordinate
//! that survives the resize — and the only one a panel can use.
//!
//! An image with no code in it is an answer, not a failure: it comes back as an
//! outcome with no symbols, and the page says so. The errors here are all about
//! the *file* — missing, unreadable, not an image.
use anyhow::{Context, Result, ensure};
use image::{ImageReader, imageops::FilterType};
use rxing::{BarcodeFormat, DecodeHints, RXingResult, helpers};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodesIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    /// The bytes are not a picture in a format this build can decode.
    NotImage,
    /// A picture too big to scan, however small the file holding it is.
    TooManyPixels,
}

impl std::fmt::Display for CodesIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("Input file could not be found"),
            Self::Read => f.write_str("Could not read the file"),
            Self::NotFile => f.write_str("Please choose a file, not a folder"),
            Self::TooLarge => {
                f.write_str("Images are limited to 32 MiB. Please choose a smaller one")
            }
            Self::NotImage => {
                f.write_str("This file is not an image, or uses a format this build cannot read")
            }
            Self::TooManyPixels => {
                f.write_str("This image is larger than 40 megapixels. Please scale it down first")
            }
        }
    }
}
impl std::error::Error for CodesIssue {}

/// A photograph holding a barcode is rarely near this; a file that is has more
/// wrong with it than its size.
pub const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
/// Recognition walks the whole picture several times, so the pixel count is the
/// real budget — a small file can still decode into an enormous image.
pub const MAX_PIXELS: u64 = 40_000_000;
/// Longest edge of the thumbnail handed back for display. Large enough to read
/// a code on screen, small enough to hand across a channel as plain pixels.
pub const PREVIEW_EDGE: u32 = 900;
/// Offered in the open dialog, and what routes a dropped file to this tool.
pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "tga", "qoi", "pnm", "pgm",
    "ppm", "pbm",
];

/// Whether a path names an image this tool can open, by its extension alone.
/// That is the only signal available before reading the file.
pub fn claims(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The symbologies this build recognizes. Their labels are the names printed on
/// specifications and packaging, so they are never translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbology {
    Aztec,
    Codabar,
    Code39,
    Code93,
    Code128,
    DataMatrix,
    DxFilmEdge,
    Ean8,
    Ean13,
    Itf,
    MaxiCode,
    MicroQrCode,
    Pdf417,
    QrCode,
    RectangularMicroQrCode,
    Rss14,
    RssExpanded,
    Telepen,
    UpcA,
    UpcE,
    UpcEanExtension,
    /// A format rxing knows and this list does not: shown rather than dropped.
    Other,
}

impl Symbology {
    pub fn label(self) -> &'static str {
        match self {
            Self::Aztec => "Aztec",
            Self::Codabar => "Codabar",
            Self::Code39 => "Code 39",
            Self::Code93 => "Code 93",
            Self::Code128 => "Code 128",
            Self::DataMatrix => "Data Matrix",
            Self::DxFilmEdge => "DX Film Edge",
            Self::Ean8 => "EAN-8",
            Self::Ean13 => "EAN-13",
            Self::Itf => "ITF",
            Self::MaxiCode => "MaxiCode",
            Self::MicroQrCode => "Micro QR Code",
            Self::Pdf417 => "PDF417",
            Self::QrCode => "QR Code",
            Self::RectangularMicroQrCode => "rMQR",
            Self::Rss14 => "GS1 DataBar",
            Self::RssExpanded => "GS1 DataBar Expanded",
            Self::Telepen => "Telepen",
            Self::UpcA => "UPC-A",
            Self::UpcE => "UPC-E",
            Self::UpcEanExtension => "UPC/EAN extension",
            Self::Other => "Barcode",
        }
    }

    /// Whether the symbol is a matrix rather than a row of bars. A 1D code is
    /// found as a line across it, so its box has height only by convention.
    pub fn matrix(self) -> bool {
        matches!(
            self,
            Self::Aztec
                | Self::DataMatrix
                | Self::MaxiCode
                | Self::MicroQrCode
                | Self::Pdf417
                | Self::QrCode
                | Self::RectangularMicroQrCode
        )
    }

    fn from_format(format: &BarcodeFormat) -> Self {
        match format {
            BarcodeFormat::AZTEC => Self::Aztec,
            BarcodeFormat::CODABAR => Self::Codabar,
            BarcodeFormat::CODE_39 => Self::Code39,
            BarcodeFormat::CODE_93 => Self::Code93,
            BarcodeFormat::CODE_128 => Self::Code128,
            BarcodeFormat::DATA_MATRIX => Self::DataMatrix,
            BarcodeFormat::DXFilmEdge => Self::DxFilmEdge,
            BarcodeFormat::EAN_8 => Self::Ean8,
            BarcodeFormat::EAN_13 => Self::Ean13,
            BarcodeFormat::ITF => Self::Itf,
            BarcodeFormat::MAXICODE => Self::MaxiCode,
            BarcodeFormat::MICRO_QR_CODE => Self::MicroQrCode,
            BarcodeFormat::PDF_417 => Self::Pdf417,
            BarcodeFormat::QR_CODE => Self::QrCode,
            BarcodeFormat::RECTANGULAR_MICRO_QR_CODE => Self::RectangularMicroQrCode,
            BarcodeFormat::RSS_14 => Self::Rss14,
            BarcodeFormat::RSS_EXPANDED => Self::RssExpanded,
            BarcodeFormat::TELEPEN => Self::Telepen,
            BarcodeFormat::UPC_A => Self::UpcA,
            BarcodeFormat::UPC_E => Self::UpcE,
            BarcodeFormat::UPC_EAN_EXTENSION => Self::UpcEanExtension,
            _ => Self::Other,
        }
    }
}

/// Where a symbol sits, as a fraction of the image: 0.0 is the left or top
/// edge, 1.0 the right or bottom one. Independent of the size anything is
/// displayed at, which is the point.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Area {
    fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub symbology: Symbology,
    /// What the symbol says, already decoded from the character set it declared.
    pub text: String,
    pub area: Area,
}

/// The image at a size worth drawing, as plain RGBA rows. Kept as bytes rather
/// than as a file path so that the picture is decoded once, on the worker, and
/// never again on the event loop.
#[derive(Clone, Debug)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ScanOutcome {
    pub source: PathBuf,
    /// The image's own size, not the preview's.
    pub width: u32,
    pub height: u32,
    /// In reading order: top to bottom, then left to right.
    pub symbols: Vec<Symbol>,
    pub preview: Preview,
    pub elapsed: Duration,
}

/// Read an image and recognize every code in it.
pub fn scan(path: &Path) -> Result<ScanOutcome> {
    let started = Instant::now();
    let source = path.canonicalize().context(CodesIssue::InputMissing)?;
    let meta = std::fs::metadata(&source).context(CodesIssue::Read)?;
    ensure!(meta.is_file(), CodesIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, CodesIssue::TooLarge);
    let bytes = std::fs::read(&source).context(CodesIssue::Read)?;
    ensure!(bytes.len() as u64 <= MAX_INPUT_BYTES, CodesIssue::TooLarge);

    // Ask for the dimensions before decoding: a header is cheap to read, and a
    // picture over the budget must not be allocated to find that out.
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .context(CodesIssue::Read)?;
    let (width, height) = reader.into_dimensions().context(CodesIssue::NotImage)?;
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS,
        CodesIssue::TooManyPixels
    );
    let image = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .context(CodesIssue::Read)?
        .decode()
        .context(CodesIssue::NotImage)?;

    let preview = preview(&image);
    let luma = image.into_luma8();
    let symbols = recognize(luma.into_raw(), width, height);
    Ok(ScanOutcome {
        source,
        width,
        height,
        symbols,
        preview,
        elapsed: started.elapsed(),
    })
}

/// Every code the recognizer can find, in reading order and without repeats.
fn recognize(luma: Vec<u8>, width: u32, height: u32) -> Vec<Symbol> {
    let mut hints = DecodeHints {
        // Worth the extra passes: this runs on a file someone chose, once, and
        // a photograph of a label rarely decodes on the first try.
        TryHarder: Some(true),
        // A code printed light-on-dark is still that code.
        AlsoInverted: Some(true),
        ..Default::default()
    };
    let found = helpers::detect_multiple_in_luma_with_hints(
        luma.clone(),
        width,
        height,
        &mut hints.clone(),
    )
    .or_else(|_| {
        // The multiple reader wants a clean picture. This one rescales and
        // filters the image before giving up, which is what saves a photograph
        // holding a single code.
        helpers::detect_in_luma_filtered_with_hints(luma, width, height, None, &mut hints)
            .map(|single| vec![single])
    })
    .unwrap_or_default();

    let mut symbols: Vec<Symbol> = found
        .iter()
        .map(|result| present(result, width, height))
        .collect();
    // Reading order, in the picture rather than in the order they were found.
    symbols.sort_by(|a, b| {
        let (left, right) = (a.area.center(), b.area.center());
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.total_cmp(&right.0))
    });
    symbols.dedup_by(|a, b| same(a, b));
    symbols
}

/// Whether two symbols are one code reported twice: the same content, from the
/// same place. Two copies of a label in one picture are two symbols, so position
/// has to be part of the answer.
fn same(a: &Symbol, b: &Symbol) -> bool {
    let (left, right) = (a.area.center(), b.area.center());
    a.symbology == b.symbology
        && a.text == b.text
        && (left.0 - right.0).abs() < 0.02
        && (left.1 - right.1).abs() < 0.02
}

fn present(result: &RXingResult, width: u32, height: u32) -> Symbol {
    Symbol {
        symbology: Symbology::from_format(result.getBarcodeFormat()),
        text: result.getText().to_owned(),
        area: area(result, width, height),
    }
}

/// The box around a symbol's locator points, as a fraction of the image. A 1D
/// code is found as a line across the bars, so its box can be flat; the caller
/// decides how thick a line it draws.
fn area(result: &RXingResult, width: u32, height: u32) -> Area {
    let points = result.getPoints();
    let (Some(first), true) = (points.first(), width > 0 && height > 0) else {
        return Area::default();
    };
    let (mut left, mut top) = (first.x, first.y);
    let (mut right, mut bottom) = (first.x, first.y);
    for point in points {
        left = left.min(point.x);
        right = right.max(point.x);
        top = top.min(point.y);
        bottom = bottom.max(point.y);
    }
    let (w, h) = (width as f32, height as f32);
    let x = (left / w).clamp(0.0, 1.0);
    let y = (top / h).clamp(0.0, 1.0);
    Area {
        x,
        y,
        width: (right / w).clamp(0.0, 1.0) - x,
        height: (bottom / h).clamp(0.0, 1.0) - y,
    }
}

/// The picture at display size. Never upscaled: a tiny image shown large is the
/// panel's business, and enlarging it here would only cost memory.
fn preview(image: &image::DynamicImage) -> Preview {
    let scaled = if image.width() > PREVIEW_EDGE || image.height() > PREVIEW_EDGE {
        image.resize(PREVIEW_EDGE, PREVIEW_EDGE, FilterType::Triangle)
    } else {
        image.clone()
    };
    let rgba = scaled.into_rgba8();
    Preview {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    }
}
