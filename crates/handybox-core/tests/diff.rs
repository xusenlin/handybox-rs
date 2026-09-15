use handybox_core::tools::diff::{
    self, CHANGED, Change, DiffIssue, Limit, MAX_INPUT_BYTES, MAX_ROWS, ORIGINAL, Options, Row,
    Side,
};
use std::fs;

fn side<'a>(name: &'a str, text: &'a str) -> Side<'a> {
    Side { name, text }
}

fn compare<'a>(left: &'a str, right: &'a str, options: Options) -> diff::DiffOutcome {
    diff::compare(side(ORIGINAL, left), side(CHANGED, right), options).unwrap()
}

/// The rows a reader sees, as `marker old new text`, which is compact enough to
/// assert a whole comparison against.
fn sketch(outcome: &diff::DiffOutcome) -> Vec<String> {
    outcome
        .rows
        .iter()
        .map(|row| match row {
            Row::Gap(skipped) => format!("~ {skipped}"),
            Row::Line {
                change,
                old,
                new,
                text,
            } => format!(
                "{} {} {} {text}",
                match change {
                    Change::Equal => " ",
                    Change::Insert => "+",
                    Change::Delete => "-",
                },
                old.map(|n| n.to_string()).unwrap_or_default(),
                new.map(|n| n.to_string()).unwrap_or_default(),
            ),
        })
        .collect()
}

#[test]
fn numbers_every_line_on_the_side_it_belongs_to() {
    let outcome = compare("a\nb\nc\n", "a\nB\nc\nd\n", Options::default());
    assert_eq!(
        sketch(&outcome),
        ["  1 1 a", "- 2  b", "+  2 B", "  3 3 c", "+  4 d",]
    );
    assert_eq!((outcome.added, outcome.removed), (2, 1));
    assert_eq!((outcome.old_lines, outcome.new_lines), (3, 4));
    assert!(!outcome.identical());
}

#[test]
fn identical_sides_produce_no_changes_and_no_diff_to_export() {
    let outcome = compare("一行\n二行\n", "一行\n二行\n", Options::default());
    assert!(outcome.identical());
    assert_eq!(outcome.hunks, 0);
    assert!(outcome.unified.is_empty());
    assert_eq!(outcome.similarity, 1.0);
    // Nothing changed, but the reader still sees the document.
    assert_eq!(outcome.rows.len(), 2);
    // Line endings are not the subject of a text comparison.
    assert!(compare("a\r\nb\r\n", "a\nb", Options::default()).identical());
}

#[test]
fn ignoring_whitespace_changes_what_matches_but_not_what_is_shown() {
    let (left, right) = ("  value = 1\nkept\n", "value   =  1\nkept\n");
    let strict = compare(left, right, Options::default());
    assert_eq!((strict.added, strict.removed), (1, 1));
    let relaxed = compare(
        left,
        right,
        Options {
            ignore_whitespace: true,
            ..Default::default()
        },
    );
    assert!(relaxed.identical());
    // The original indentation survives: only the comparison was relaxed.
    assert_eq!(sketch(&relaxed)[0], "  1 1   value = 1");
}

#[test]
fn only_changes_leaves_out_the_untouched_runs_and_says_how_many() {
    let left: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let right = left.replace("line 15\n", "changed 15\n");
    let outcome = compare(
        &left,
        &right,
        Options {
            changes_only: true,
            ..Default::default()
        },
    );
    let rows = sketch(&outcome);
    assert_eq!(rows.first().map(String::as_str), Some("~ 11"));
    assert_eq!(rows.last().map(String::as_str), Some("~ 12"));
    assert!(rows.contains(&"- 15  line 15".to_owned()));
    assert!(rows.contains(&"+  15 changed 15".to_owned()));
    // Three lines of context on each side of the one change.
    assert_eq!(rows.len(), 2 + 6 + 2);
    // The same run without the option shows every line and no gaps.
    let full = compare(&left, &right, Options::default());
    assert_eq!(full.rows.len(), 31);
    assert!(!full.rows.iter().any(|row| matches!(row, Row::Gap(_))));
}

#[test]
fn writes_a_unified_diff_other_tools_can_read() {
    let outcome = diff::compare(
        side("before.txt", "a\nb\nc\n"),
        side("after.txt", "a\nc\nd\n"),
        Options::default(),
    )
    .unwrap();
    assert_eq!(
        outcome.unified,
        "--- before.txt\n+++ after.txt\n@@ -1,3 +1,3 @@\n a\n-b\n c\n+d\n"
    );
    assert_eq!(outcome.hunks, 1);
}

#[test]
fn an_empty_side_is_a_comparison_but_two_empty_sides_are_not() {
    let added = compare("", "new\n", Options::default());
    assert_eq!((added.added, added.removed), (1, 0));
    // A hunk that holds no lines on one side names the line before it, not a
    // line that does not exist.
    assert!(
        added.unified.contains("@@ -0,0 +1,1 @@"),
        "{}",
        added.unified
    );
    let removed = compare("gone\n", "", Options::default());
    assert!(removed.unified.contains("@@ -1,1 +0,0 @@"));
    let error =
        diff::compare(side(ORIGINAL, ""), side(CHANGED, ""), Options::default()).unwrap_err();
    assert_eq!(error.downcast_ref::<DiffIssue>(), Some(&DiffIssue::Empty));
}

#[test]
fn refuses_a_side_larger_than_the_cap_and_bounds_the_rows_it_returns() {
    let oversized = "x\n".repeat(MAX_INPUT_BYTES as usize / 2 + 1);
    let error = diff::compare(
        side(ORIGINAL, &oversized),
        side(CHANGED, ""),
        Options::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<DiffIssue>(),
        Some(&DiffIssue::TooLarge)
    );
    let long: String = (0..MAX_ROWS + 500).map(|n| format!("{n}\n")).collect();
    let outcome = compare(&long, &long, Options::default());
    assert_eq!(outcome.rows.len(), MAX_ROWS);
    assert_eq!(outcome.limit, Some(Limit::Rows));
}

#[test]
fn loads_only_readable_utf8_files_within_the_limit() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("left.txt");
    fs::write(&source, "hello\n").unwrap();
    assert_eq!(diff::load(&source).unwrap().text, "hello\n");
    assert!(diff::load(directory.path()).is_err());
    assert!(diff::load(&directory.path().join("missing.txt")).is_err());
    let binary = directory.path().join("binary.txt");
    fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(
        diff::load(&binary).unwrap_err().downcast_ref::<DiffIssue>(),
        Some(&DiffIssue::NotText)
    );
}

#[test]
fn export_never_overwrites_either_side_and_requires_a_diff_extension() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left.diff");
    let right = directory.path().join("right.diff");
    fs::write(&left, "original left").unwrap();
    fs::write(&right, "original right").unwrap();
    let sources = vec![left.clone(), right.clone()];
    assert_eq!(
        diff::export(&sources, &right, "replacement")
            .unwrap_err()
            .downcast_ref::<DiffIssue>(),
        Some(&DiffIssue::SourceOverwrite)
    );
    assert_eq!(fs::read_to_string(&right).unwrap(), "original right");
    let invalid = directory.path().join("result.txt");
    assert!(diff::export(&sources, &invalid, "").is_err());
    assert!(!invalid.exists());
    let output = directory.path().join("result.patch");
    diff::export(&sources, &output, "--- a\n+++ b\n").unwrap();
    assert_eq!(fs::read_to_string(&output).unwrap(), "--- a\n+++ b\n");
    // Text typed into an editor has no file to protect.
    diff::export(&[], &output, "replaced").unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), "replaced");
}
