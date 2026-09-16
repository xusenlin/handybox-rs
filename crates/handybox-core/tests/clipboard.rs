//! What the clipboard workspace keeps, what it refuses, and what it throws away.
use handybox_core::tools::clipboard::{
    self, Content, History, Kind, MAX_ITEMS, MAX_TEXT_BYTES, MAX_TOTAL_BYTES, Picture, Recorded,
    SUMMARY_CHARS, Thumbnail,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn text(value: &str) -> Content {
    Content::Text(value.to_owned())
}

/// A picture of a given weight. Only its bytes matter to the workspace.
fn picture(fill: u8, bytes: usize) -> Content {
    Content::Image(Picture {
        width: 800,
        height: 600,
        png: vec![fill; bytes],
        thumbnail: Thumbnail {
            width: 4,
            height: 1,
            rgba: vec![fill; 16],
        },
    })
}

#[test]
fn the_same_content_copied_twice_is_one_item_back_at_the_top() {
    let mut history = History::default();
    let now = Instant::now();
    let first = history.record(text("alpha"), now).id();
    history.record(text("beta"), now);
    assert_eq!(history.len(), 2);
    assert_eq!(history.items()[0].content, text("beta"));

    // Copying something again is how a clipboard gets used; it is not a second
    // item, and the one it already has moves back to where it is being used.
    let later = now + Duration::from_secs(30);
    let recorded = history.record(text("alpha"), later);
    assert_eq!(recorded, Recorded::Moved(first));
    assert_eq!(history.len(), 2);
    assert_eq!(history.items()[0].id, first);
    assert_eq!(history.items()[0].age(later), Duration::ZERO);
    // A picture with the same pixels is the same item too.
    history.record(picture(7, 64), later);
    assert_eq!(history.record(picture(7, 64), later), Recorded::Moved(3));
    assert_eq!(history.len(), 3);
}

#[test]
fn the_oldest_leaves_when_the_workspace_runs_out_of_room() {
    let mut history = History::default();
    let now = Instant::now();
    for index in 0..MAX_ITEMS + 5 {
        history.record(text(&format!("item {index}")), now);
    }
    assert_eq!(history.len(), MAX_ITEMS);
    // The five that left are the five that had been there longest.
    assert_eq!(
        history.items()[0].content,
        text(&format!("item {}", MAX_ITEMS + 4))
    );
    assert_eq!(history.items()[MAX_ITEMS - 1].content, text("item 5"));

    // Pictures run the workspace out of bytes long before it runs out of rows.
    let mut history = History::default();
    let weight = MAX_TOTAL_BYTES / 4;
    for fill in 0..6u8 {
        history.record(picture(fill, weight), now);
    }
    assert!(history.len() < 6 && history.bytes() <= MAX_TOTAL_BYTES);
    // One item may exceed the whole budget: what was just captured always stays.
    let mut history = History::default();
    history.record(picture(1, MAX_TOTAL_BYTES * 2), now);
    assert_eq!(history.len(), 1);
}

#[test]
fn removing_and_clearing_give_back_the_room_they_took() {
    let mut history = History::default();
    let now = Instant::now();
    let id = history.record(picture(3, 4096), now).id();
    history.record(text("still here"), now);
    let full = history.bytes();
    assert!(full > 4096);

    assert!(history.remove(id));
    assert!(
        !history.remove(id),
        "removing twice is not an error to repeat"
    );
    assert_eq!(history.bytes(), full - 4096 - 16);
    assert!(history.get(id).is_none());
    assert_eq!(history.len(), 1);

    history.clear();
    assert!(history.is_empty() && history.bytes() == 0);

    // A picture is its PNG; the preview kept beside it costs memory but is not
    // part of the item, so the totals under a list add up to what the rows say.
    let mut history = History::default();
    history.record(picture(5, 1024), now);
    assert_eq!(history.size(), 1024);
    assert_eq!(history.bytes(), 1024 + 16);
}

#[test]
fn empty_and_oversized_captures_are_refused() {
    assert!(clipboard::accept(text("")).is_err());
    assert!(clipboard::accept(Content::Files(Vec::new())).is_err());
    assert!(
        clipboard::accept(text(" ")).is_ok(),
        "whitespace is content"
    );
    let huge = "x".repeat(MAX_TEXT_BYTES + 1);
    let refused = clipboard::accept(text(&huge)).unwrap_err();
    assert_eq!(
        refused.downcast_ref::<clipboard::ClipboardIssue>().copied(),
        Some(clipboard::ClipboardIssue::TooLarge)
    );
}

#[test]
fn a_row_shows_one_bounded_line_whatever_was_copied() {
    // The whole text on one line, not its first line: a first line is often a
    // comment marker or an opening brace, which says nothing about what was
    // copied. Line breaks and runs of whitespace collapse to single spaces.
    let summary = text("\n\n  Dear Alice,\nthe rest   of the letter\n").summary();
    assert_eq!(summary, "Dear Alice, the rest of the letter");
    // Control characters would break the row they are shown in.
    assert_eq!(text("a\tb\u{7}c").summary(), "a b c");
    let long = text(&"字".repeat(SUMMARY_CHARS + 40)).summary();
    assert_eq!(long.chars().count(), SUMMARY_CHARS + 1);
    assert!(long.ends_with('…'));
    // Whitespace has no line worth showing; the caller says so in its own words.
    assert!(text("   \n  ").summary().is_empty());

    // Files read as their names, and a picture as the only thing there is to
    // say about it before you look at it.
    let files = Content::Files(vec![
        PathBuf::from("/tmp/report.pdf"),
        PathBuf::from("/tmp/notes.txt"),
    ]);
    assert_eq!(files.summary(), "report.pdf  ·  notes.txt");
    assert_eq!(files.kind(), Kind::Files);
    assert_eq!(picture(1, 8).summary(), "800 × 600");
    assert!(picture(1, 8).text().is_none(), "a picture is not text");
    // Pasted somewhere else, file references are their paths.
    assert_eq!(files.text().unwrap(), "/tmp/report.pdf\n/tmp/notes.txt");
}

#[test]
fn file_references_survive_the_uri_they_arrive_as() {
    assert_eq!(
        clipboard::path_from_reference("file:///Users/ada/My%20Notes/%E4%B8%AD%E6%96%87.txt"),
        PathBuf::from("/Users/ada/My Notes/中文.txt")
    );
    // A plain path is already a path.
    assert_eq!(
        clipboard::path_from_reference(" /tmp/plain.txt\n"),
        PathBuf::from("/tmp/plain.txt")
    );
    // The slash before a drive letter belongs to the URI, not to the path.
    assert_eq!(
        clipboard::path_from_reference("file:///C:/Users/ada/notes.txt"),
        PathBuf::from("C:/Users/ada/notes.txt")
    );
    // A per cent sign that is not an escape is part of the name.
    assert_eq!(
        clipboard::path_from_reference("/tmp/100%.txt"),
        PathBuf::from("/tmp/100%.txt")
    );
}

#[test]
fn saving_a_picture_writes_the_bytes_that_were_collected_and_insists_on_png() {
    let directory = tempfile::tempdir().unwrap();
    // A picture leaves as the very PNG that goes back on the clipboard.
    let png = b"\x89PNG\r\n\x1a\nnot really, but these are the bytes kept".to_vec();
    let wrong = directory.path().join("picture.jpg");
    assert!(clipboard::export(&png, &wrong).is_err());
    assert!(!wrong.exists(), "a refused export leaves nothing behind");

    let output = directory.path().join("picture.png");
    clipboard::export(&png, &output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), png);
    // The extension is matched however it is written, and an existing file is
    // replaced whole rather than appended to.
    let upper = directory.path().join("picture.PNG");
    clipboard::export(b"second", &upper).unwrap();
    clipboard::export(b"third", &upper).unwrap();
    assert_eq!(std::fs::read(upper).unwrap(), b"third");
}

#[test]
fn text_is_measured_the_way_it_is_read() {
    let stats = clipboard::stats("第一行\nsecond line\n");
    assert_eq!(stats.lines, 2);
    assert_eq!(stats.characters, 16);
    assert_eq!(stats.bytes, 22);
    assert_eq!(clipboard::stats("").lines, 0);
}
