//! What a picture is read as, what comes out of a recipe, and what an export
//! refuses to overwrite.
use handybox_core::tools::images::{self, Format, ImageIssue, MAX_WIDTH, Recipe};
use image::{ImageFormat, ImageReader, Rgba, RgbaImage};
use std::{io::Cursor, path::Path};

/// Reproducible run-profile measurement; fixture creation is outside the timer.
#[test]
#[ignore = "manual image loading benchmark"]
fn benchmark_large_image_load() {
    let directory = tempfile::tempdir().unwrap();
    let mut seed = 42_u32;
    let pixels = image::RgbImage::from_fn(2400, 1500, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
    });
    for extension in ["png", "jpg"] {
        let path = directory.path().join(format!("noise.{extension}"));
        pixels.save(&path).unwrap();
        let start = std::time::Instant::now();
        let opened = images::load(&path).unwrap();
        eprintln!(
            "{extension}: {} bytes, {:?}, preview {} × {}",
            opened.bytes,
            start.elapsed(),
            opened.preview.width,
            opened.preview.height
        );
    }
}

/// A picture with something in it: a flat colour compresses to nothing and
/// would make every size comparison meaningless.
fn picture(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            (x * 7 % 256) as u8,
            (y * 13 % 256) as u8,
            ((x + y) * 3 % 256) as u8,
            255,
        ])
    })
}

fn write(path: &Path, width: u32, height: u32, format: ImageFormat) {
    let picture = image::DynamicImage::ImageRgba8(picture(width, height));
    // JPEG has no alpha channel, which is the tool's own reason for flattening
    // a picture before it encodes one.
    let picture = if format == ImageFormat::Jpeg {
        image::DynamicImage::ImageRgb8(picture.to_rgb8())
    } else {
        picture
    };
    picture
        .save_with_format(path, format)
        .expect("the fixture should be written");
}

#[test]
fn a_picture_is_read_with_what_it_is_rather_than_what_it_is_called() {
    let directory = tempfile::tempdir().unwrap();
    // The extension says one thing; the content decides.
    let path = directory.path().join("photo.bin");
    write(&path, 120, 80, ImageFormat::Png);
    let opened = images::load(&path).unwrap();
    assert_eq!((opened.width, opened.height), (120, 80));
    assert_eq!(opened.format, "PNG");
    assert_eq!(opened.colour, "RGBA 8-bit");
    assert!(opened.bytes > 0);
    // A picture nobody photographed says nothing about itself.
    assert!(opened.details.is_empty());
    // The thumbnail is the picture at drawing size, never larger than it was.
    assert_eq!(
        (opened.preview.width, opened.preview.height),
        (120, 80),
        "a small picture is not enlarged"
    );
    assert_eq!(opened.preview.rgba.len(), 120 * 80 * 4);
    assert!(images::claims(Path::new("holiday.JPEG")));
    assert!(!images::claims(Path::new("notes.txt")));
}

#[test]
fn every_format_writes_what_it_says_and_reads_back_the_same_size() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.png");
    write(&path, 64, 48, ImageFormat::Png);
    let opened = images::load(&path).unwrap();

    for (format, signature) in [
        (Format::Png, &b"\x89PNG"[..]),
        (Format::Jpeg, &b"\xff\xd8"[..]),
        (Format::Bmp, &b"BM"[..]),
        (Format::Tiff, &b"II"[..]),
    ] {
        let rendered = images::render(
            &opened,
            &Recipe {
                format,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rendered.format, format);
        assert!(
            rendered.bytes.starts_with(signature),
            "{} did not write its own header",
            format.label()
        );
        assert_eq!((rendered.width, rendered.height), (64, 48));
        // And it is a picture again on the way back in.
        let back = ImageReader::new(Cursor::new(&rendered.bytes))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!((back.width(), back.height()), (64, 48));
    }
    assert!(Format::Png.lossless() && !Format::Jpeg.lossless());
}

#[test]
fn a_resize_keeps_the_proportions_and_refuses_a_width_that_is_not_one() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("wide.png");
    write(&path, 200, 100, ImageFormat::Png);
    let opened = images::load(&path).unwrap();

    let rendered = images::render(
        &opened,
        &Recipe {
            width: Some(50),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((rendered.width, rendered.height), (50, 25));

    // The picture's own width is not a resize, and neither is no width at all.
    for width in [None, Some(200)] {
        let rendered = images::render(
            &opened,
            &Recipe {
                width,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((rendered.width, rendered.height), (200, 100));
    }

    for width in [0, MAX_WIDTH + 1] {
        let refused = images::render(
            &opened,
            &Recipe {
                width: Some(width),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<ImageIssue>().copied(),
            Some(ImageIssue::InvalidWidth)
        );
    }
}

#[test]
fn optimizing_a_png_only_ever_makes_it_smaller_and_changes_no_pixel() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.png");
    write(&path, 160, 120, ImageFormat::Png);
    let opened = images::load(&path).unwrap();
    let plain = images::render(
        &opened,
        &Recipe {
            optimize: false,
            ..Default::default()
        },
    )
    .unwrap();
    let optimized = images::render(&opened, &Recipe::default()).unwrap();
    assert!(
        optimized.size() <= plain.size(),
        "the optimizer added bytes"
    );
    // Lossless means lossless: the same picture comes back out.
    let before = image::load_from_memory(&plain.bytes).unwrap().to_rgba8();
    let after = image::load_from_memory(&optimized.bytes)
        .unwrap()
        .to_rgba8();
    assert_eq!(before, after);
}

#[test]
fn quality_is_what_a_jpeg_costs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.png");
    write(&path, 200, 200, ImageFormat::Png);
    let opened = images::load(&path).unwrap();
    let render = |quality| {
        images::render(
            &opened,
            &Recipe {
                format: Format::Jpeg,
                quality,
                ..Default::default()
            },
        )
        .unwrap()
        .size()
    };
    assert!(render(40) < render(95), "quality bought nothing");
}

#[test]
fn an_export_writes_the_rendered_bytes_and_refuses_the_picture_it_came_from() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.png");
    write(&source, 40, 40, ImageFormat::Png);
    let opened = images::load(&source).unwrap();
    let rendered = images::render(&opened, &Recipe::default()).unwrap();

    // The extension has to match the format that was rendered.
    let wrong = directory.path().join("result.jpg");
    assert_eq!(
        images::export(&rendered, &wrong, &source)
            .unwrap_err()
            .downcast_ref::<ImageIssue>()
            .copied(),
        Some(ImageIssue::InvalidExtension)
    );
    assert!(!wrong.exists(), "a refused export leaves nothing behind");
    // Nor may it replace the picture that was opened.
    assert_eq!(
        images::export(&rendered, &source, &source)
            .unwrap_err()
            .downcast_ref::<ImageIssue>()
            .copied(),
        Some(ImageIssue::SourceOverwrite)
    );

    let output = directory.path().join("result.png");
    images::export(&rendered, &output, &source).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), rendered.bytes);
    // A JPEG may be spelled either way round.
    let jpeg = images::render(
        &opened,
        &Recipe {
            format: Format::Jpeg,
            ..Default::default()
        },
    )
    .unwrap();
    images::export(&jpeg, &directory.path().join("result.jpeg"), &source).unwrap();
    assert_eq!(images::suggested_name(&opened, Format::Jpeg), "source.jpg");
    assert_eq!(images::suggested_name(&opened, Format::Tiff), "source.tiff");
}

#[test]
fn a_folder_offers_the_pictures_in_it_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(&root.join("beach.png"), 30, 20, ImageFormat::Png);
    write(&root.join("Apple.png"), 10, 10, ImageFormat::Png);
    write(&root.join("cliff.jpg"), 40, 10, ImageFormat::Jpeg);
    std::fs::write(root.join("notes.txt"), b"not a picture").unwrap();
    // A name that promises a picture and is not one stays out of the list: the
    // header is read, not the extension.
    std::fs::write(root.join("broken.png"), b"nope").unwrap();
    std::fs::create_dir_all(root.join("inner")).unwrap();
    write(&root.join("inner/hidden.png"), 10, 10, ImageFormat::Png);

    let listing = images::list(root).unwrap();
    let names: Vec<&str> = listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    // By name with case aside, the way a file manager shows them — and the
    // folder itself only, never what is inside the folders in it.
    assert_eq!(names, ["Apple.png", "beach.png", "cliff.jpg"]);
    assert_eq!(listing.left_out, 0);
    let beach = &listing.entries[1];
    assert_eq!((beach.width, beach.height), (30, 20));
    assert!(beach.bytes > 0);
    assert_eq!(listing.folder, root.canonicalize().unwrap());

    // A file is not a folder, however many pictures are beside it.
    let refused = images::list(&root.join("beach.png")).unwrap_err();
    assert_eq!(
        refused.downcast_ref::<ImageIssue>().copied(),
        Some(ImageIssue::NotFolder)
    );
}

#[test]
fn a_batch_writes_into_a_folder_and_replaces_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("holiday.png");
    write(&source, 40, 40, ImageFormat::Png);
    let into = directory.path().join("out");
    std::fs::create_dir_all(&into).unwrap();
    let opened = images::load(&source).unwrap();
    let rendered = images::render(&opened, &Recipe::default()).unwrap();

    // The picture's own name, with the extension of the format that was made.
    let written = images::export_into(&rendered, &into, &source).unwrap();
    assert_eq!(written, into.join("holiday.png"));
    assert_eq!(std::fs::read(&written).unwrap(), rendered.bytes);

    // A batch has nobody to ask, so a name that is taken is refused rather
    // than replaced.
    assert_eq!(
        images::export_into(&rendered, &into, &source)
            .unwrap_err()
            .downcast_ref::<ImageIssue>()
            .copied(),
        Some(ImageIssue::Exists)
    );
    // And the picture that was read is never the picture that is written over,
    // even when the folder someone chose is the one it came from.
    assert_eq!(
        images::export_into(&rendered, directory.path(), &source)
            .unwrap_err()
            .downcast_ref::<ImageIssue>()
            .copied(),
        Some(ImageIssue::SourceOverwrite)
    );
    // Another format beside it is a different file, and allowed.
    let jpeg = images::render(
        &opened,
        &Recipe {
            format: Format::Jpeg,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        images::export_into(&jpeg, directory.path(), &source).unwrap(),
        directory.path().join("holiday.jpg")
    );
    assert_eq!(
        images::output_path(&source, &into, Format::Tiff),
        into.join("holiday.tiff")
    );
}

#[test]
fn a_folder_a_missing_file_and_something_that_is_not_a_picture_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let issue = |path: &Path| {
        images::load(path)
            .unwrap_err()
            .downcast_ref::<ImageIssue>()
            .copied()
    };
    assert_eq!(issue(directory.path()), Some(ImageIssue::NotFile));
    assert_eq!(
        issue(&directory.path().join("nowhere.png")),
        Some(ImageIssue::InputMissing)
    );
    let text = directory.path().join("notes.png");
    std::fs::write(&text, b"this is not a picture").unwrap();
    assert_eq!(issue(&text), Some(ImageIssue::NotImage));
}

#[test]
fn previews_fit_portraits_panoramas_and_transparent_images() {
    let directory = tempfile::tempdir().unwrap();
    for (width, height, expected) in [
        (900, 1800, (360, 720)),
        (1800, 900, (720, 360)),
        (1, 1000, (1, 720)),
        (1000, 1, (720, 1)),
    ] {
        let path = directory.path().join("transparent.png");
        RgbaImage::from_pixel(width, height, Rgba([32, 128, 224, 128]))
            .save(&path)
            .unwrap();
        let picture = images::load(&path).unwrap();
        assert_eq!((picture.preview.width, picture.preview.height), expected);
        assert_eq!(
            picture.preview.rgba.len(),
            (expected.0 * expected.1 * 4) as usize
        );
        assert!(
            picture
                .preview
                .rgba
                .chunks_exact(4)
                .all(|p| p[3] == 128 && p[2].abs_diff(224) <= 2)
        );
    }
}

#[test]
fn resizing_preserves_sixteen_bit_precision() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("depth.png");
    image::ImageBuffer::from_pixel(1500, 1000, image::Luma([12345_u16]))
        .save(&path)
        .unwrap();
    let picture = images::load(&path).unwrap();
    assert_eq!((picture.preview.width, picture.preview.height), (720, 480));
    let rendered = images::render(
        &picture,
        &Recipe {
            width: Some(750),
            optimize: false,
            ..Default::default()
        },
    )
    .unwrap();
    let decoded = image::load_from_memory(&rendered.bytes).unwrap();
    assert_eq!(decoded.color(), image::ColorType::L16);
    assert_eq!(decoded.as_luma16().unwrap().get_pixel(20, 20).0, [12345]);
    let bmp = images::render(
        &picture,
        &Recipe {
            format: Format::Bmp,
            width: Some(750),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(image::load_from_memory(&bmp.bytes).unwrap().width(), 750);
}

#[test]
fn exif_orientation_applies_to_preview_and_export() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rotated.jpg");
    write(&path, 80, 40, ImageFormat::Jpeg);
    let jpeg = std::fs::read(&path).unwrap();
    // JPEG APP1 with one little-endian TIFF entry: orientation = rotate 90 CW.
    let exif: &[u8] = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut rotated = vec![0xff, 0xd8, 0xff, 0xe1];
    rotated.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    rotated.extend_from_slice(exif);
    rotated.extend_from_slice(&jpeg[2..]);
    std::fs::write(&path, rotated).unwrap();
    let picture = images::load(&path).unwrap();
    assert_eq!((picture.width, picture.height), (40, 80));
    assert_eq!((picture.preview.width, picture.preview.height), (40, 80));
    let rendered = images::render(&picture, &Recipe::default()).unwrap();
    let decoded = image::load_from_memory(&rendered.bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (40, 80));
}

#[cfg(unix)]
#[test]
fn batch_does_not_replace_a_dangling_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("photo.png");
    write(&source, 10, 10, ImageFormat::Png);
    let into = directory.path().join("out");
    std::fs::create_dir(&into).unwrap();
    let target = into.join("photo.png");
    std::os::unix::fs::symlink("missing.png", &target).unwrap();
    let rendered = images::render(&images::load(&source).unwrap(), &Recipe::default()).unwrap();
    assert_eq!(
        images::export_into(&rendered, &into, &source)
            .unwrap_err()
            .downcast_ref::<ImageIssue>(),
        Some(&ImageIssue::Exists)
    );
    assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
}
