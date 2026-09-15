use handybox_core::tools::codes::{
    self, CodesIssue, MAX_INPUT_BYTES, PREVIEW_EDGE, ScanOutcome, Symbology,
};
use image::{GenericImage, GrayImage, Luma};
use rxing::{BarcodeFormat, MultiFormatWriter, Writer};
use std::{fs, path::Path, path::PathBuf};

/// A barcode drawn as an image, which is the only input this tool takes. The
/// writer produces a module matrix; a test has to put it on a canvas itself.
fn render(text: &str, format: BarcodeFormat, width: i32, height: i32) -> GrayImage {
    let matrix = MultiFormatWriter
        .encode(text, &format, width, height)
        .expect("the fixture should encode");
    let mut image = GrayImage::from_pixel(matrix.getWidth(), matrix.getHeight(), Luma([255]));
    for y in 0..matrix.getHeight() {
        for x in 0..matrix.getWidth() {
            if matrix.get(x, y) {
                image.put_pixel(x, y, Luma([0]));
            }
        }
    }
    image
}

/// A white page with codes pasted onto it, the way a scanned document looks.
fn page(width: u32, height: u32, codes: &[(&GrayImage, u32, u32)]) -> GrayImage {
    let mut canvas = GrayImage::from_pixel(width, height, Luma([255]));
    for (code, x, y) in codes {
        canvas
            .copy_from(*code, *x, *y)
            .expect("the code should fit");
    }
    canvas
}

fn save(directory: &Path, name: &str, image: &GrayImage) -> PathBuf {
    let path = directory.join(name);
    image.save(&path).expect("the fixture should be written");
    path
}

fn texts(outcome: &ScanOutcome) -> Vec<&str> {
    outcome
        .symbols
        .iter()
        .map(|symbol| symbol.text.as_str())
        .collect()
}

#[test]
fn reads_a_qr_code_and_says_where_it_is() {
    let directory = tempfile::tempdir().unwrap();
    // Non-ASCII content is the case a barcode reader gets wrong: the symbol
    // declares its own character set, and the text has to survive it.
    let code = render("https://例子.cn/工具箱", BarcodeFormat::QR_CODE, 200, 200);
    let path = save(
        directory.path(),
        "qr.png",
        &page(600, 400, &[(&code, 300, 100)]),
    );

    let outcome = codes::scan(&path).unwrap();
    assert_eq!(outcome.source, path.canonicalize().unwrap());
    assert_eq!((outcome.width, outcome.height), (600, 400));
    assert_eq!(outcome.symbols.len(), 1);
    let symbol = &outcome.symbols[0];
    assert_eq!(symbol.symbology, Symbology::QrCode);
    assert_eq!(symbol.symbology.label(), "QR Code");
    assert!(symbol.symbology.matrix());
    assert_eq!(symbol.text, "https://例子.cn/工具箱");
    // The code sits in the right half, below the top edge, and is square-ish.
    assert!(symbol.area.x > 0.5, "{:?}", symbol.area);
    assert!(
        symbol.area.y > 0.2 && symbol.area.y < 0.4,
        "{:?}",
        symbol.area
    );
    assert!((symbol.area.width - 0.3).abs() < 0.1, "{:?}", symbol.area);
    assert!(
        (symbol.area.height - 0.45).abs() < 0.15,
        "{:?}",
        symbol.area
    );
    // Small enough to show as it is, so the preview is the picture itself.
    assert_eq!((outcome.preview.width, outcome.preview.height), (600, 400));
    assert_eq!(outcome.preview.rgba.len(), 600 * 400 * 4);
}

#[test]
fn finds_every_code_in_one_picture_in_reading_order() {
    let directory = tempfile::tempdir().unwrap();
    let qr = render("second", BarcodeFormat::QR_CODE, 160, 160);
    let ean = render("9781861972712", BarcodeFormat::EAN_13, 300, 120);
    // The QR sits lower on the page, so it is reported second however the
    // recognizer happened to come across it.
    let path = save(
        directory.path(),
        "sheet.png",
        &page(700, 500, &[(&ean, 60, 40), (&qr, 260, 280)]),
    );

    let outcome = codes::scan(&path).unwrap();
    assert_eq!(texts(&outcome), ["9781861972712", "second"]);
    assert_eq!(outcome.symbols[0].symbology, Symbology::Ean13);
    assert_eq!(outcome.symbols[0].symbology.label(), "EAN-13");
    // A row of bars is found as a line across them, so its box may be flat.
    assert!(!outcome.symbols[0].symbology.matrix());
    assert!(outcome.symbols[0].area.y < outcome.symbols[1].area.y);
}

#[test]
fn reads_a_code_printed_light_on_dark() {
    let directory = tempfile::tempdir().unwrap();
    let mut inverted = render("inverted", BarcodeFormat::QR_CODE, 240, 240);
    inverted
        .pixels_mut()
        .for_each(|pixel| pixel.0[0] = 255 - pixel.0[0]);
    let mut canvas = GrayImage::from_pixel(400, 400, Luma([0]));
    canvas.copy_from(&inverted, 80, 80).unwrap();
    let path = save(directory.path(), "inverted.png", &canvas);

    assert_eq!(texts(&codes::scan(&path).unwrap()), ["inverted"]);
}

#[test]
fn an_image_with_no_code_in_it_is_an_answer_rather_than_a_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = save(directory.path(), "blank.png", &page(320, 240, &[]));

    let outcome = codes::scan(&path).unwrap();
    assert!(outcome.symbols.is_empty());
    assert_eq!((outcome.width, outcome.height), (320, 240));
    assert_eq!(outcome.preview.rgba.len(), 320 * 240 * 4);
}

#[test]
fn scales_the_preview_down_without_moving_anything_in_it() {
    let directory = tempfile::tempdir().unwrap();
    let code = render("large", BarcodeFormat::QR_CODE, 400, 400);
    let path = save(
        directory.path(),
        "large.png",
        &page(1800, 1200, &[(&code, 1200, 200)]),
    );

    let outcome = codes::scan(&path).unwrap();
    // The reported size is the image's own; only the preview was resized, and
    // it keeps the aspect ratio the positions are expressed in.
    assert_eq!((outcome.width, outcome.height), (1800, 1200));
    assert_eq!(outcome.preview.width, PREVIEW_EDGE);
    assert_eq!(outcome.preview.height, PREVIEW_EDGE * 1200 / 1800);
    assert_eq!(
        outcome.preview.rgba.len() as u32,
        outcome.preview.width * outcome.preview.height * 4
    );
    let area = outcome.symbols[0].area;
    assert!(area.x > 0.65 && area.x < 0.75, "{area:?}");
    assert!(area.y > 0.1 && area.y < 0.25, "{area:?}");
}

#[test]
fn only_opens_files_that_are_images_within_the_limits() {
    let directory = tempfile::tempdir().unwrap();
    let issue = |path: &Path| {
        codes::scan(path)
            .unwrap_err()
            .downcast_ref::<CodesIssue>()
            .copied()
    };
    assert_eq!(
        issue(&directory.path().join("missing.png")),
        Some(CodesIssue::InputMissing)
    );
    assert_eq!(issue(directory.path()), Some(CodesIssue::NotFile));
    // The name is not the evidence: what is inside it is.
    let disguised = directory.path().join("notes.png");
    fs::write(&disguised, "just text, despite the extension").unwrap();
    assert_eq!(issue(&disguised), Some(CodesIssue::NotImage));
    let oversized = directory.path().join("huge.png");
    fs::write(&oversized, vec![0u8; MAX_INPUT_BYTES as usize + 1]).unwrap();
    assert_eq!(issue(&oversized), Some(CodesIssue::TooLarge));
}

#[test]
fn refuses_a_picture_with_more_pixels_than_it_will_walk() {
    let directory = tempfile::tempdir().unwrap();
    // Uniform, so it costs almost nothing on disk: the pixel count is the
    // budget, and a small file can still carry an enormous picture.
    let path = save(directory.path(), "enormous.png", &page(7000, 6000, &[]));
    assert!(fs::metadata(&path).unwrap().len() < MAX_INPUT_BYTES);
    assert_eq!(
        codes::scan(&path)
            .unwrap_err()
            .downcast_ref::<CodesIssue>()
            .copied(),
        Some(CodesIssue::TooManyPixels)
    );
}

#[test]
fn claims_the_image_files_it_offers_to_open() {
    assert!(codes::claims(Path::new("/tmp/ticket.PNG")));
    assert!(codes::claims(Path::new("/tmp/photo.jpeg")));
    assert!(codes::claims(Path::new("/tmp/scan.webp")));
    assert!(!codes::claims(Path::new("/tmp/notes.md")));
    assert!(!codes::claims(Path::new("/tmp/archive.png.age")));
    assert!(!codes::claims(Path::new("/tmp/no-extension")));
}
