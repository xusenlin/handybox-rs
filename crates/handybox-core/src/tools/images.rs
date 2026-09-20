//! Read a picture, see what it is, and write it back at another size or in
//! another format.
//!
//! Two steps, deliberately. Rendering produces the bytes and says how large
//! they turned out; exporting writes those very bytes. A tool that only
//! rendered while saving would ask someone to choose a quality without ever
//! showing what the choice cost.
//!
//! The pixels are decoded once. Everything after that — the thumbnail on
//! screen, a resize, an encode — works from the same decoded image, because
//! opening a 20-megapixel photograph twice is the one cost worth avoiding here.
//!
//! What leaves is pixels and nothing else: the encoders write no EXIF, so an
//! exported picture carries no camera, no serial number and no coordinates.
//! That is a property worth keeping rather than a limitation to apologize for.
use anyhow::{Context, Result, anyhow, bail, ensure};
use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::{
    DynamicImage, ImageEncoder, ImageFormat, ImageReader,
    codecs::{bmp::BmpEncoder, jpeg::JpegEncoder, png::PngEncoder, tiff::TiffEncoder},
    metadata::Orientation,
};
use nom_exif::{Exif, ExifTag};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    /// The bytes are not a picture in a format this build can decode.
    NotImage,
    /// A picture too big to work on, however small the file holding it is.
    TooManyPixels,
    /// A file was chosen where a folder is needed: this tool works on a folder.
    NotFolder,
    /// There is a file of that name already. A batch never asks and therefore
    /// never replaces anything.
    Exists,
    /// A width that is not a width: zero, or more than any picture has.
    InvalidWidth,
    Encode,
    InvalidExtension,
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for ImageIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("Input file could not be found"),
            Self::Read => f.write_str("Could not read the file"),
            Self::NotFile => f.write_str("Please choose a file, not a folder"),
            Self::TooLarge => {
                f.write_str("Images are limited to 64 MiB. Please choose a smaller one")
            }
            Self::NotImage => {
                f.write_str("This file is not an image, or uses a format this build cannot read")
            }
            Self::TooManyPixels => {
                f.write_str("This image is larger than 50 megapixels. Please scale it down first")
            }
            Self::NotFolder => f.write_str("Please choose a folder, not a file"),
            Self::Exists => f.write_str("A file of that name is already there"),
            Self::InvalidWidth => f.write_str("Width must be between 1 and 20000 pixels"),
            Self::Encode => f.write_str("Could not write the picture in that format"),
            Self::InvalidExtension => {
                f.write_str("The file name must end with the chosen format's extension")
            }
            Self::SourceOverwrite => f.write_str("Cannot replace the picture that was opened"),
            Self::CreateOutput => f.write_str("Could not create a file in the chosen folder"),
            Self::WriteOutput => f.write_str("Could not write the picture"),
            Self::SaveOutput => f.write_str("Could not save to the chosen file"),
        }
    }
}
impl std::error::Error for ImageIssue {}

/// A photograph is rarely near this; a file that is has more wrong with it than
/// its size.
pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
/// Working on a picture means holding it uncompressed: fifty megapixels is
/// already 200 MiB of RGBA before anything is resized or encoded.
pub const MAX_PIXELS: u64 = 50_000_000;
/// Widths past this are a typo rather than an intention.
pub const MAX_WIDTH: u32 = 20_000;
/// Pictures one folder may offer. A camera roll can hold more; the list says
/// so rather than growing until the page cannot be drawn.
pub const MAX_ENTRIES: usize = 1_000;
/// Longest edge of the thumbnail handed to the page.
pub const PREVIEW_EDGE: u32 = 720;
/// How hard the PNG optimizer looks. The optimizer's own default walks far more
/// filter and compression combinations for a few per cent, and this runs while
/// someone waits.
pub const OPTIMIZE_LEVEL: u8 = 2;
/// Offered in the open dialog, and what this tool can decode.
pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "tga", "qoi", "pnm", "pgm",
    "ppm", "pbm",
];

/// Whether a path names an image this tool can open, by its extension alone.
pub fn claims(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// One picture in a folder, as far as its header goes. Listing reads no pixels:
/// a folder of five hundred photographs would be gigabytes, and what a list
/// needs is the name, the size and the shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

/// What a folder holds.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub folder: PathBuf,
    /// By name, the way a file manager shows them.
    pub entries: Vec<Entry>,
    /// Pictures beyond the cap, which are in the folder but not in the list.
    pub left_out: usize,
}

/// What a picture can be written as. Four, because these are the formats worth
/// converting *to*: one lossless, one for photographs, one for print and
/// scanning workflows, and one that everything old can open. WebP is missing on
/// purpose — this build can read it but the pure-Rust encoder does not exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
    Tiff,
    Bmp,
}

impl Format {
    /// Printed on specifications rather than translated.
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
        }
    }

    /// Whether the format keeps every pixel exactly. Only JPEG does not, which
    /// is also the only one with a quality to choose.
    pub fn lossless(self) -> bool {
        self != Self::Jpeg
    }

    /// Both names a file may end with, so an export is not refused for the
    /// spelling of an extension.
    fn accepts(self, extension: &str) -> bool {
        match self {
            Self::Jpeg => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
            Self::Tiff => {
                extension.eq_ignore_ascii_case("tiff") || extension.eq_ignore_ascii_case("tif")
            }
            other => extension.eq_ignore_ascii_case(other.extension()),
        }
    }
}

/// The picture at a size worth drawing, as plain RGBA rows.
#[derive(Clone, Debug)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A fact about the picture, named by what it is rather than by the words used
/// to say it: the caller has the language, this has the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Camera,
    Lens,
    Taken,
    Exposure,
    Aperture,
    Iso,
    FocalLength,
    Software,
    Location,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub tag: Tag,
    pub value: String,
}

/// An opened picture: what it is, what it says about itself, and the pixels
/// everything else works from.
pub struct Picture {
    pub source: PathBuf,
    pub width: u32,
    pub height: u32,
    /// The size of the file on disk, not of the pixels in memory.
    pub bytes: u64,
    /// The format it was read as, printed rather than translated.
    pub format: &'static str,
    /// Channels and depth, in the same spirit.
    pub colour: &'static str,
    /// What the camera recorded about it. Empty for a picture that says
    /// nothing, which is most pictures that were ever edited.
    pub details: Vec<Detail>,
    pub preview: Preview,
    /// Decoded once, and already turned the right way up.
    image: DynamicImage,
}

impl std::fmt::Debug for Picture {
    /// Everything but the pixels. A derived one would print every byte of a
    /// photograph the first time anything logged it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("source", &self.source)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes)
            .field("format", &self.format)
            .field("colour", &self.colour)
            .field("details", &self.details)
            .finish_non_exhaustive()
    }
}

impl Picture {
    /// The pixels, for a caller that wants to look rather than to re-decode.
    pub fn image(&self) -> &DynamicImage {
        &self.image
    }
}

/// What to make of a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub format: Format,
    /// The width to scale to, keeping the proportions. None leaves the size
    /// alone, which is also what conversion alone means.
    pub width: Option<u32>,
    /// JPEG only, 1–100.
    pub quality: u8,
    /// PNG only: run the lossless optimizer over the encoded bytes.
    pub optimize: bool,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            format: Format::Png,
            width: None,
            quality: 85,
            optimize: true,
        }
    }
}

/// The bytes a recipe produced, and what they cost.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub elapsed: Duration,
}

impl Rendered {
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

/// Every picture in a folder, by name. The folder itself only — a batch acts on
/// what someone can see in the list, and a recursive sweep would quietly act on
/// what they cannot.
pub fn list(folder: &Path) -> Result<Listing> {
    let folder = folder.canonicalize().context(ImageIssue::InputMissing)?;
    ensure!(
        std::fs::metadata(&folder)
            .context(ImageIssue::Read)?
            .is_dir(),
        ImageIssue::NotFolder
    );
    let listing = std::fs::read_dir(&folder).context(ImageIssue::Read)?;
    let mut entries = Vec::new();
    let mut left_out = 0;
    for entry in listing.flatten() {
        let path = entry.path();
        if !claims(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_INPUT_BYTES {
            continue;
        }
        // The header, not the picture: this is what keeps listing a folder of
        // photographs a matter of milliseconds.
        let Ok((width, height)) = dimensions(&path) else {
            continue;
        };
        if entries.len() >= MAX_ENTRIES {
            left_out += 1;
            continue;
        }
        entries.push(Entry {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            width,
            height,
            bytes: meta.len(),
        });
    }
    // By name, case aside: a folder listed differently from the file manager
    // beside it is a folder nobody can find anything in.
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Listing {
        folder,
        entries,
        left_out,
    })
}

/// A picture's size without decoding it.
fn dimensions(path: &Path) -> Result<(u32, u32)> {
    ImageReader::open(path)
        .context(ImageIssue::Read)?
        .with_guessed_format()
        .context(ImageIssue::Read)?
        .into_dimensions()
        .context(ImageIssue::NotImage)
}

/// Open a picture: decode it, turn it the right way up, read what the camera
/// left in it, and keep a thumbnail to draw.
pub fn load(path: &Path) -> Result<Picture> {
    let source = path.canonicalize().context(ImageIssue::InputMissing)?;
    let meta = std::fs::metadata(&source).context(ImageIssue::Read)?;
    ensure!(meta.is_file(), ImageIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, ImageIssue::TooLarge);
    let bytes = std::fs::read(&source).context(ImageIssue::Read)?;

    // The header first: a picture over the budget must not be allocated to
    // discover that it is over the budget.
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .context(ImageIssue::Read)?;
    let format = reader.format();
    let (width, height) = reader.into_dimensions().context(ImageIssue::NotImage)?;
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS,
        ImageIssue::TooManyPixels
    );
    let mut image = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .context(ImageIssue::Read)?
        .decode()
        .context(ImageIssue::NotImage)?;

    // A photograph taken sideways is stored sideways with a note about it.
    // Applying the note here is what makes every later step — the thumbnail, a
    // resize, the exported file — agree with what the camera showed.
    let exif = nom_exif::MediaSource::from_memory(bytes)
        .and_then(|source| nom_exif::MediaParser::new().parse_exif(source))
        .map(Exif::from)
        .ok();
    if let Some(orientation) = exif
        .as_ref()
        .and_then(|exif| exif.get(ExifTag::Orientation))
        .and_then(|value| value.as_u16().map(u32::from).or_else(|| value.as_u32()))
        .and_then(|value| u8::try_from(value).ok().and_then(Orientation::from_exif))
    {
        image.apply_orientation(orientation);
    }
    let preview = preview(&image)?;
    Ok(Picture {
        source,
        width: image.width(),
        height: image.height(),
        bytes: meta.len(),
        format: format.map_or("", name),
        colour: colour(&image),
        details: exif.as_ref().map(details).unwrap_or_default(),
        preview,
        image,
    })
}

/// Produce the output bytes. Nothing is written to disk here: what comes back
/// is what an export would save, so its size can be shown before anyone
/// commits to it.
pub fn render(picture: &Picture, recipe: &Recipe) -> Result<Rendered> {
    let started = Instant::now();
    if let Some(width) = recipe.width {
        ensure!(width > 0 && width <= MAX_WIDTH, ImageIssue::InvalidWidth);
    }
    let resized = match recipe.width {
        Some(width) if width != picture.width => Some(resize(&picture.image, width)?),
        _ => None,
    };
    let image = resized.as_ref().unwrap_or(&picture.image);
    let bytes = encode(image, recipe)?;
    Ok(Rendered {
        bytes,
        width: image.width(),
        height: image.height(),
        format: recipe.format,
        elapsed: started.elapsed(),
    })
}

/// Write rendered bytes to the file someone chose. The extension is not
/// corrected silently: the native dialog has already asked about overwriting
/// the path as it was typed.
pub fn export(rendered: &Rendered, destination: &Path, source: &Path) -> Result<()> {
    ensure!(
        destination
            .extension()
            .is_some_and(|extension| rendered.format.accepts(&extension.to_string_lossy())),
        ImageIssue::InvalidExtension
    );
    if let Ok(existing) = destination.canonicalize() {
        ensure!(
            existing != source.canonicalize().context(ImageIssue::InputMissing)?,
            ImageIssue::SourceOverwrite
        );
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).context(ImageIssue::CreateOutput)?;
    temp.write_all(&rendered.bytes)
        .context(ImageIssue::WriteOutput)?;
    temp.as_file().sync_all().context(ImageIssue::WriteOutput)?;
    temp.persist(destination).context(ImageIssue::SaveOutput)?;
    Ok(())
}

/// Where a picture lands when a batch writes it into a folder: its own name,
/// with the extension the chosen format writes.
pub fn output_path(source: &Path, into: &Path, format: Format) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());
    into.join(format!("{stem}.{}", format.extension()))
}

/// Write one picture of a batch into a folder.
///
/// A batch has nobody to ask, so it replaces nothing: a name that is taken is
/// reported and skipped, and the picture that was read is never the picture
/// that is written over. Everything else is the ordinary atomic write.
pub fn export_into(rendered: &Rendered, into: &Path, source: &Path) -> Result<PathBuf> {
    let destination = output_path(source, into, rendered.format);
    if let Ok(existing) = destination.canonicalize() {
        ensure!(
            existing != source.canonicalize().context(ImageIssue::InputMissing)?,
            ImageIssue::SourceOverwrite
        );
        bail!(ImageIssue::Exists);
    }
    // The final create must also be exclusive: a preflight exists check alone
    // could replace a file created by another process during the write.
    let mut temp = tempfile::NamedTempFile::new_in(into).context(ImageIssue::CreateOutput)?;
    temp.write_all(&rendered.bytes)
        .context(ImageIssue::WriteOutput)?;
    temp.as_file().sync_all().context(ImageIssue::WriteOutput)?;
    temp.persist_noclobber(&destination).map_err(|error| {
        let issue = if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            ImageIssue::Exists
        } else {
            ImageIssue::SaveOutput
        };
        anyhow!(error).context(issue)
    })?;
    Ok(destination)
}

/// The name an export would suggest: the picture's own, with the extension the
/// chosen format writes.
pub fn suggested_name(picture: &Picture, format: Format) -> String {
    let stem = picture
        .source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_owned());
    format!("{stem}.{}", format.extension())
}

/// Scale to a width, keeping the proportions.
///
/// Retains the original channel layout and bit depth.
fn resize(image: &DynamicImage, width: u32) -> Result<DynamicImage> {
    let height =
        ((u64::from(width) * u64::from(image.height())) / u64::from(image.width().max(1))).max(1);
    ensure!(
        u64::from(width) * height <= MAX_PIXELS,
        ImageIssue::TooManyPixels
    );
    resize_pixels(image, width, height as u32, FilterType::Lanczos3)
}

// Borrow the original pixel layout: no full-size RGBA copy, and 16-bit
// sources keep their precision when resized for export.
fn resize_pixels(
    image: &DynamicImage,
    width: u32,
    height: u32,
    filter: FilterType,
) -> Result<DynamicImage> {
    let mut target = DynamicImage::new(width, height, image.color());
    Resizer::new()
        .resize(
            image,
            &mut target,
            &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter)),
        )
        .map_err(|error| anyhow!("{error}"))
        .context(ImageIssue::Encode)?;
    Ok(target)
}

fn encode(image: &DynamicImage, recipe: &Recipe) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    match recipe.format {
        Format::Png => {
            PngEncoder::new_with_quality(
                &mut bytes,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(
                image.as_bytes(),
                image.width(),
                image.height(),
                image.color().into(),
            )
            .context(ImageIssue::Encode)?;
            if recipe.optimize {
                // A failure here is not a failure to convert: the picture is
                // already encoded, and the optimizer only had nothing to add.
                if let Ok(smaller) = oxipng::optimize_from_memory(
                    &bytes,
                    &oxipng::Options::from_preset(OPTIMIZE_LEVEL),
                ) && smaller.len() < bytes.len()
                {
                    bytes = smaller;
                }
            }
        }
        // JPEG has no alpha; an image that has one is flattened onto nothing
        // rather than refused, which is what every other tool does with it.
        Format::Jpeg => JpegEncoder::new_with_quality(&mut bytes, recipe.quality.clamp(1, 100))
            .encode_image(&DynamicImage::ImageRgb8(image.to_rgb8()))
            .context(ImageIssue::Encode)?,
        Format::Tiff => TiffEncoder::new(Cursor::new(&mut bytes))
            .write_image(
                image.as_bytes(),
                image.width(),
                image.height(),
                image.color().into(),
            )
            .context(ImageIssue::Encode)?,
        Format::Bmp => {
            // BMP supports 8-bit channels. Retaining precision during resize
            // must not make a high-depth source impossible to export as BMP.
            let converted = (!matches!(
                image.color(),
                image::ColorType::L8
                    | image::ColorType::La8
                    | image::ColorType::Rgb8
                    | image::ColorType::Rgba8
            ))
            .then(|| DynamicImage::ImageRgba8(image.to_rgba8()));
            let image = converted.as_ref().unwrap_or(image);
            BmpEncoder::new(&mut bytes)
                .write_image(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )
                .context(ImageIssue::Encode)?;
        }
    }
    Ok(bytes)
}

/// The picture at display size. Never upscaled: a small picture shown large is
/// the page's business, and enlarging it here would only cost memory.
fn preview(image: &DynamicImage) -> Result<Preview> {
    let edge = image.width().max(image.height());
    let rgba = if edge > PREVIEW_EDGE {
        let width =
            (u64::from(image.width()) * u64::from(PREVIEW_EDGE) / u64::from(edge)).max(1) as u32;
        let height =
            (u64::from(image.height()) * u64::from(PREVIEW_EDGE) / u64::from(edge)).max(1) as u32;
        resize_pixels(image, width, height, FilterType::Bilinear)?.into_rgba8()
    } else {
        image.to_rgba8()
    };
    Ok(Preview {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

fn name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::WebP => "WebP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
        ImageFormat::Tga => "TGA",
        ImageFormat::Qoi => "QOI",
        ImageFormat::Pnm => "PNM",
        _ => "",
    }
}

fn colour(image: &DynamicImage) -> &'static str {
    use image::ColorType::*;
    match image.color() {
        L8 => "Grayscale 8-bit",
        La8 => "Grayscale + alpha 8-bit",
        Rgb8 => "RGB 8-bit",
        Rgba8 => "RGBA 8-bit",
        L16 => "Grayscale 16-bit",
        La16 => "Grayscale + alpha 16-bit",
        Rgb16 => "RGB 16-bit",
        Rgba16 => "RGBA 16-bit",
        Rgb32F => "RGB 32-bit float",
        Rgba32F => "RGBA 32-bit float",
        _ => "",
    }
}

/// What the camera wrote, in the order someone reads it. Values are kept as
/// they were recorded: a shutter speed is the camera's own fraction, not a
/// rounding of it.
fn details(exif: &Exif) -> Vec<Detail> {
    let read = |tag: ExifTag| -> Option<String> {
        let value = exif.get(tag)?.to_string();
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    let mut details = Vec::new();
    let camera = match (read(ExifTag::Make), read(ExifTag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    };
    let mut push = |tag: Tag, value: Option<String>| {
        if let Some(value) = value {
            details.push(Detail { tag, value });
        }
    };
    push(Tag::Camera, camera);
    push(Tag::Lens, read(ExifTag::LensModel));
    push(Tag::Taken, read(ExifTag::DateTimeOriginal));
    push(Tag::Exposure, read(ExifTag::ExposureTime));
    push(Tag::Aperture, read(ExifTag::FNumber));
    push(Tag::Iso, read(ExifTag::ISOSpeedRatings));
    push(Tag::FocalLength, read(ExifTag::FocalLength));
    push(Tag::Software, read(ExifTag::Software));
    // Coordinates are shown because they are in the file and someone about to
    // share it should know that. The export carries none of this.
    let location = match (read(ExifTag::GPSLatitude), read(ExifTag::GPSLongitude)) {
        (Some(latitude), Some(longitude)) => Some(format!("{latitude}, {longitude}")),
        _ => None,
    };
    push(Tag::Location, location);
    details
}
