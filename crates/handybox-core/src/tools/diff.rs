//! Local line-by-line comparison of two texts.
//!
//! The comparison runs over a normalized *key* per line rather than over the
//! line itself, so "ignore whitespace" changes what counts as the same line
//! without changing what is shown: every row still carries the original text and
//! the line number it has on its own side.
//!
//! Two shapes come out of one run. [`Row`]s are what a reader looks at, in the
//! order a diff is read — removals before the additions that replace them. The
//! unified diff is what leaves the application, and it is always built with
//! context, whether or not the panel is showing the unchanged lines.
use anyhow::{Context, Result, ensure};
use similar::{Algorithm, DiffOp, DiffTag, capture_diff_slices_deadline, group_diff_ops};
use std::{
    borrow::Cow,
    fmt::Write as _,
    fs,
    io::Write as _,
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    /// Neither side holds anything to compare.
    Empty,
    NotText,
    InvalidExtension,
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for DiffIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("Input file could not be found"),
            Self::Read => f.write_str("Could not read the file"),
            Self::NotFile => f.write_str("Please choose a file, not a folder"),
            Self::TooLarge => {
                f.write_str("Each side is limited to 2 MiB. Please choose a smaller text")
            }
            Self::Empty => f.write_str("There is nothing to compare yet"),
            Self::NotText => f.write_str("This file is not UTF-8 text"),
            Self::InvalidExtension => {
                f.write_str("Exported files must use the .diff or .patch extension")
            }
            Self::SourceOverwrite => {
                f.write_str("Cannot overwrite a file being compared. Choose a different path")
            }
            Self::CreateOutput => f.write_str("Could not create a file in the destination folder"),
            Self::WriteOutput => f.write_str("Could not write the diff"),
            Self::SaveOutput => f.write_str("Could not save the destination file"),
        }
    }
}
impl std::error::Error for DiffIssue {}

/// Per side. Each one is held in an editor and laid out as a single text item,
/// so this cap is a rendering budget as much as a comparison one.
pub const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024;
/// Rows handed to the panel. The list is virtualized, but the model behind it is
/// not, so a comparison of two large documents is bounded here instead.
pub const MAX_ROWS: usize = 20_000;
/// Unchanged lines kept around every change, in the unified diff and in the
/// panel when only changes are shown.
pub const CONTEXT: usize = 3;
/// Myers is quadratic in the worst case. Passing the deadline does not fail a
/// run: the algorithm stops looking for a smaller answer and reports what is
/// left as one replacement.
pub const TIME_BUDGET: Duration = Duration::from_secs(5);
/// Offered in the open dialog. Any UTF-8 text can be compared; these are only
/// the extensions worth putting in front of the "all files" filter.
pub const EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "json", "csv", "tsv", "log", "toml", "yaml", "yml", "xml", "html",
    "css", "js", "ts", "rs", "py", "go", "java", "c", "h", "cpp", "sh", "sql", "ini", "conf",
    "diff", "patch",
];
/// A unified diff is a format other tools read, so it keeps its own extensions.
pub const EXPORT_EXTENSIONS: &[&str] = &["diff", "patch"];

/// What the unified diff header calls a side that was typed rather than opened.
/// Not translated: the header is part of a machine-readable format.
pub const ORIGINAL: &str = "original";
pub const CHANGED: &str = "changed";

/// One side of the comparison, with the name it goes by in the diff header.
#[derive(Clone, Copy, Debug)]
pub struct Side<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Treat two lines that differ only in spacing as the same line. Indentation
    /// and trailing spaces are rarely the change anyone is looking for.
    pub ignore_whitespace: bool,
    /// Keep only the changes and [`CONTEXT`] lines around each of them.
    pub changes_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Equal,
    Insert,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Line {
        change: Change,
        /// 1-based line number on the side the line belongs to. A removed line
        /// has no number on the right, an added one none on the left.
        old: Option<usize>,
        new: Option<usize>,
        text: String,
    },
    /// Unchanged lines left out between two changes. The caller names it: how
    /// many lines were skipped is data, the sentence around it is not.
    Gap(usize),
}

/// Why a run produced less than the whole comparison. `None` on a full result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Rows,
    Time,
}

#[derive(Debug)]
pub struct DiffOutcome {
    pub rows: Vec<Row>,
    /// What copy and export produce. Empty when the two sides are identical:
    /// there is no such thing as a unified diff of nothing.
    pub unified: String,
    pub added: usize,
    pub removed: usize,
    /// Groups of changes, each with its own `@@` header in the unified diff.
    pub hunks: usize,
    /// Lines shared by both sides, over the length of both: 1.0 is identical.
    pub similarity: f32,
    pub old_lines: usize,
    pub new_lines: usize,
    pub elapsed: Duration,
    pub limit: Option<Limit>,
}

impl DiffOutcome {
    pub fn identical(&self) -> bool {
        self.added == 0 && self.removed == 0
    }
}

#[derive(Debug)]
pub struct LoadedText {
    pub source: PathBuf,
    pub text: String,
}

/// Compare two texts line by line.
pub fn compare(left: Side<'_>, right: Side<'_>, options: Options) -> Result<DiffOutcome> {
    let started = Instant::now();
    ensure!(
        left.text.len() as u64 <= MAX_INPUT_BYTES && right.text.len() as u64 <= MAX_INPUT_BYTES,
        DiffIssue::TooLarge
    );
    ensure!(
        !left.text.is_empty() || !right.text.is_empty(),
        DiffIssue::Empty
    );
    // `lines` drops the line terminators, so a file written on Windows and the
    // same file written on Unix compare as equal rather than as every line
    // changed. That is the answer people want from a text comparison.
    let old: Vec<&str> = left.text.lines().collect();
    let new: Vec<&str> = right.text.lines().collect();
    let old_keys: Vec<Cow<'_, str>> = old.iter().map(|line| key(line, options)).collect();
    let new_keys: Vec<Cow<'_, str>> = new.iter().map(|line| key(line, options)).collect();
    let ops = capture_diff_slices_deadline(
        Algorithm::Myers,
        &old_keys,
        &new_keys,
        Instant::now().checked_add(TIME_BUDGET),
    );
    let exhausted = started.elapsed() >= TIME_BUDGET;

    let mut added = 0;
    let mut removed = 0;
    let mut matched = 0;
    for op in &ops {
        match op.tag() {
            DiffTag::Equal => matched += op.old_range().len(),
            _ => {
                removed += op.old_range().len();
                added += op.new_range().len();
            }
        }
    }
    let groups = group_diff_ops(ops.clone(), CONTEXT);
    let unified = unified(&groups, &old, &new, left.name, right.name);
    let mut rows = if options.changes_only {
        grouped_rows(&groups, &old, &new)
    } else {
        plain_rows(&ops, &old, &new)
    };
    let capped = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);

    Ok(DiffOutcome {
        rows,
        unified,
        added,
        removed,
        hunks: groups.len(),
        similarity: match old.len() + new.len() {
            0 => 1.0,
            total => 2.0 * matched as f32 / total as f32,
        },
        old_lines: old.len(),
        new_lines: new.len(),
        elapsed: started.elapsed(),
        limit: match (capped, exhausted) {
            (true, _) => Some(Limit::Rows),
            (false, true) => Some(Limit::Time),
            _ => None,
        },
    })
}

/// Bounded read of a file the caller chose. The text is returned, not the lines:
/// the editor owns that side of the comparison from here on.
pub fn load(path: &Path) -> Result<LoadedText> {
    let source = path.canonicalize().context(DiffIssue::InputMissing)?;
    let meta = fs::metadata(&source).context(DiffIssue::Read)?;
    ensure!(meta.is_file(), DiffIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, DiffIssue::TooLarge);
    let bytes = fs::read(&source).context(DiffIssue::Read)?;
    ensure!(bytes.len() as u64 <= MAX_INPUT_BYTES, DiffIssue::TooLarge);
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!(DiffIssue::NotText))
        .context(DiffIssue::NotText)?;
    Ok(LoadedText { source, text })
}

/// Write beside the destination, then atomically persist. Never replace either
/// of the documents being compared. The caller owns overwrite confirmation
/// (native save dialog).
pub fn export(sources: &[PathBuf], destination: &Path, text: &str) -> Result<()> {
    ensure!(
        destination.extension().is_some_and(|extension| {
            EXPORT_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        }),
        DiffIssue::InvalidExtension
    );
    if let Ok(existing) = destination.canonicalize() {
        for source in sources {
            ensure!(
                existing != source.canonicalize().context(DiffIssue::InputMissing)?,
                DiffIssue::SourceOverwrite
            );
        }
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).context(DiffIssue::CreateOutput)?;
    temp.write_all(text.as_bytes())
        .context(DiffIssue::WriteOutput)?;
    temp.as_file().sync_all().context(DiffIssue::WriteOutput)?;
    temp.persist(destination).context(DiffIssue::SaveOutput)?;
    Ok(())
}

/// What a line is compared as. Borrowed unless an option asks for a rewrite.
fn key<'a>(line: &'a str, options: Options) -> Cow<'a, str> {
    if options.ignore_whitespace {
        // Collapse runs of whitespace as well as trimming: re-indenting a line
        // and re-wrapping the spaces inside it are the same kind of non-change.
        Cow::Owned(line.split_whitespace().collect::<Vec<_>>().join(" "))
    } else {
        Cow::Borrowed(line)
    }
}

/// Every line of both sides, changed or not.
fn plain_rows(ops: &[DiffOp], old: &[&str], new: &[&str]) -> Vec<Row> {
    let mut rows = Vec::new();
    for op in ops {
        append(&mut rows, op, old, new);
        if rows.len() >= MAX_ROWS {
            break;
        }
    }
    rows
}

/// Only the changes and the context around them, with a gap standing in for
/// each run of unchanged lines that was left out — including before the first
/// change and after the last.
fn grouped_rows(groups: &[Vec<DiffOp>], old: &[&str], new: &[&str]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut cursor = 0;
    for group in groups {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        gap(&mut rows, first.old_range().start - cursor);
        for op in group {
            append(&mut rows, op, old, new);
        }
        cursor = last.old_range().end;
        if rows.len() >= MAX_ROWS {
            return rows;
        }
    }
    gap(&mut rows, old.len() - cursor);
    rows
}

fn gap(rows: &mut Vec<Row>, skipped: usize) {
    if skipped > 0 {
        rows.push(Row::Gap(skipped));
    }
}

/// One operation's rows. A replacement reads as its removals followed by the
/// additions that stand in for them, which is the order a diff is read in.
fn append(rows: &mut Vec<Row>, op: &DiffOp, old: &[&str], new: &[&str]) {
    let (tag, old_range, new_range) = op.as_tag_tuple();
    if tag == DiffTag::Equal {
        for (offset, line) in old[old_range.clone()].iter().enumerate() {
            rows.push(Row::Line {
                change: Change::Equal,
                old: Some(old_range.start + offset + 1),
                new: Some(new_range.start + offset + 1),
                text: (*line).to_owned(),
            });
        }
        return;
    }
    for (offset, line) in old[old_range.clone()].iter().enumerate() {
        rows.push(Row::Line {
            change: Change::Delete,
            old: Some(old_range.start + offset + 1),
            new: None,
            text: (*line).to_owned(),
        });
    }
    for (offset, line) in new[new_range.clone()].iter().enumerate() {
        rows.push(Row::Line {
            change: Change::Insert,
            old: None,
            new: Some(new_range.start + offset + 1),
            text: (*line).to_owned(),
        });
    }
}

/// The unified diff every other tool understands: one `@@` header per group of
/// changes, then that group's lines prefixed with a space, `-` or `+`.
fn unified(groups: &[Vec<DiffOp>], old: &[&str], new: &[&str], left: &str, right: &str) -> String {
    let mut out = String::new();
    if groups.is_empty() {
        return out;
    }
    let _ = writeln!(out, "--- {left}");
    let _ = writeln!(out, "+++ {right}");
    for group in groups {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let _ = writeln!(
            out,
            "@@ -{} +{} @@",
            span(first.old_range().start..last.old_range().end),
            span(first.new_range().start..last.new_range().end)
        );
        for op in group {
            let (tag, old_range, new_range) = op.as_tag_tuple();
            if tag == DiffTag::Equal {
                for line in &old[old_range] {
                    let _ = writeln!(out, " {line}");
                }
                continue;
            }
            for line in &old[old_range] {
                let _ = writeln!(out, "-{line}");
            }
            for line in &new[new_range] {
                let _ = writeln!(out, "+{line}");
            }
        }
    }
    out
}

/// A unified diff names an empty range by the line before it, so a range's start
/// is only 1-based when it actually holds a line.
fn span(range: Range<usize>) -> String {
    let length = range.len();
    let start = if length == 0 {
        range.start
    } else {
        range.start + 1
    };
    format!("{start},{length}")
}
