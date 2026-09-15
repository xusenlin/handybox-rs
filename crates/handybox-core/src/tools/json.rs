//! Local JSON validation, formatting and jq-style queries.
//!
//! Input is parsed once into jaq's own value model: the query engine, the
//! formatter and the validator therefore agree on what the document is, and
//! integers larger than `f64` survive a round trip. Positions in syntax errors
//! come from a second, throw-away `serde_json` parse on the failure path only —
//! jaq's reader reports a byte offset, which is not something to show a person.
use anyhow::{Context, Result, ensure};
use jaq_core::{
    Compiler, Ctx, Vars, data,
    load::{Arena, File, Loader},
};
use jaq_json::{Val, read, write};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    Empty,
    NotText,
    /// Strict mode only: the document holds more than one top-level value.
    Multiple,
    /// 1-based position of the first character the parser could not accept.
    Syntax {
        line: usize,
        column: usize,
    },
    Filter,
    Query,
    InvalidExtension,
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for JsonIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("Input file could not be found"),
            Self::Read => f.write_str("Could not read the file"),
            Self::NotFile => f.write_str("Please choose a file, not a folder"),
            Self::TooLarge => f.write_str("JSON exceeds 4 MiB. Please choose a smaller document"),
            Self::Empty => f.write_str("There is no JSON to work with yet"),
            Self::NotText => f.write_str("This file is not UTF-8 text"),
            Self::Multiple => f.write_str(
                "This is a JSON stream, not one JSON document: it holds more than one top-level value",
            ),
            Self::Syntax { line, column } => {
                write!(f, "Invalid JSON at line {line}, column {column}")
            }
            Self::Filter => f.write_str("This jq filter could not be understood"),
            Self::Query => f.write_str("The filter stopped on this input"),
            Self::InvalidExtension => f.write_str("Exported files must use the .json extension"),
            Self::SourceOverwrite => {
                f.write_str("Cannot overwrite the source document. Choose a different path")
            }
            Self::CreateOutput => f.write_str("Could not create a file in the destination folder"),
            Self::WriteOutput => f.write_str("Could not write JSON"),
            Self::SaveOutput => f.write_str("Could not save the destination file"),
        }
    }
}
impl std::error::Error for JsonIssue {}

/// The editor holds the whole document in one text item, so this cap is a
/// rendering budget as much as a parsing one.
pub const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024;
pub const PREVIEW_CHARS: usize = 200_000;
/// A jq filter may describe an endless stream (`repeat(.)`); both bounds exist
/// so that such a filter returns a usable prefix instead of occupying the
/// worker forever. They cannot stop a filter that loops without emitting.
pub const MAX_OUTPUTS: usize = 20_000;
pub const TIME_BUDGET: Duration = Duration::from_secs(5);
/// Offered in the file dialog. `txt` is there because JSON is often saved that
/// way, but it is not enough to call a file JSON — see [`claims`].
pub const EXTENSIONS: &[&str] = &["json", "jsonl", "ndjson", "geojson", "jsonc", "txt"];
const OWNED_EXTENSIONS: &[&str] = &["json", "jsonl", "ndjson", "geojson", "jsonc"];

/// Whether a path names a JSON document by its extension alone. This is the
/// only signal available before reading it, and it is what routes a dropped or
/// command-line file to this tool rather than to the document converter.
pub fn claims(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        OWNED_EXTENSIONS
            .iter()
            .any(|owned| extension.eq_ignore_ascii_case(owned))
    })
}

/// A starting point for an empty workbench. Small on purpose: it is compiled in.
pub const SAMPLE: &str = r#"{
  "name": "HandyBox",
  "offline": true,
  "tools": [
    { "key": "documents", "ready": true },
    { "key": "json", "ready": true }
  ]
}"#;

/// No filter reads extra inputs, so the program graph is all the data a run needs.
type Data = data::JustLut<Val>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Pretty,
    Compact,
}

/// How many top-level values a document may hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// What jq reads: values written back to back are a stream, and a filter
    /// runs over each of them. JSON Lines files arrive this way.
    #[default]
    Stream,
    /// What RFC 8259 calls a JSON text: exactly one value, nothing after it.
    Single,
}

/// Why a run stopped early. `None` on a complete result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Outputs,
    Time,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
    /// More than one top-level value: a JSON stream or JSON Lines.
    Stream,
}

/// What a valid document contains, for the editor's status line.
#[derive(Clone, Copy, Debug)]
pub struct JsonSummary {
    pub kind: ValueKind,
    /// Members of the first value; zero for scalars.
    pub entries: usize,
    pub values: usize,
    pub bytes: usize,
}

#[derive(Debug)]
pub struct JsonOutcome {
    pub text: String,
    pub inputs: usize,
    pub outputs: usize,
    pub bytes: usize,
    pub elapsed: Duration,
    pub limit: Option<Limit>,
}

impl JsonOutcome {
    pub fn preview(&self) -> (String, bool) {
        let text: String = self.text.chars().take(PREVIEW_CHARS).collect();
        let truncated = text.len() < self.text.len();
        (text, truncated)
    }
}

#[derive(Debug)]
pub struct LoadedJson {
    pub source: PathBuf,
    pub text: String,
}

/// Parse without keeping the values: what the editor's status line reports.
pub fn validate(input: &str, mode: Mode) -> Result<JsonSummary> {
    let values = parse(input, mode)?;
    let first = values.first().expect("a parsed document has a value");
    Ok(JsonSummary {
        kind: if values.len() > 1 {
            ValueKind::Stream
        } else {
            kind(first)
        },
        entries: match first {
            Val::Arr(items) => items.len(),
            Val::Obj(members) => members.len(),
            _ => 0,
        },
        values: values.len(),
        bytes: input.len(),
    })
}

/// Apply `filter` to every top-level input value and render the outputs.
/// An empty filter is jq's identity, so formatting is a query like any other.
pub fn run(input: &str, filter: &str, layout: Layout, mode: Mode) -> Result<JsonOutcome> {
    let started = Instant::now();
    let inputs = parse(input, mode)?;
    let filter = filter.trim();
    let filter = compile(if filter.is_empty() { "." } else { filter })?;
    let pretty = write::Pp {
        indent: match layout {
            Layout::Pretty => Some("  ".to_owned()),
            Layout::Compact => None,
        },
        // `"key": value` when indented, `"key":value` when minified, as jq does.
        sep_space: layout == Layout::Pretty,
        ..Default::default()
    };
    let mut text = Vec::new();
    let mut outputs = 0;
    let mut limit = None;
    'inputs: for value in &inputs {
        let context = Ctx::<Data>::new(&filter.lut, Vars::new([]));
        for output in filter.id.run((context, value.clone())) {
            let value = match output {
                Ok(value) => value,
                // Never `unwrap_valr`: it answers `halt` with `process::exit`,
                // which would take the whole application down from a filter.
                Err(exception) => match exception.get_err() {
                    Ok(error) => {
                        return Err(anyhow::anyhow!("{error}")).context(JsonIssue::Query);
                    }
                    Err(_) => break 'inputs,
                },
            };
            if outputs > 0 {
                text.push(b'\n');
            }
            write::write(&mut text, &pretty, 0, &value).context(JsonIssue::WriteOutput)?;
            outputs += 1;
            if outputs >= MAX_OUTPUTS {
                limit = Some(Limit::Outputs);
                break 'inputs;
            }
            if started.elapsed() >= TIME_BUDGET {
                limit = Some(Limit::Time);
                break 'inputs;
            }
        }
    }
    // jaq keeps byte strings as they came in, so the result may not be UTF-8.
    let text = String::from_utf8_lossy(&text).into_owned();
    Ok(JsonOutcome {
        inputs: inputs.len(),
        outputs,
        bytes: text.len(),
        elapsed: started.elapsed(),
        limit,
        text,
    })
}

/// Bounded read of a file the caller chose. The text is returned, not the values:
/// the editor owns the document from here on.
pub fn load(path: &Path) -> Result<LoadedJson> {
    let source = path.canonicalize().context(JsonIssue::InputMissing)?;
    let meta = fs::metadata(&source).context(JsonIssue::Read)?;
    ensure!(meta.is_file(), JsonIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, JsonIssue::TooLarge);
    let bytes = fs::read(&source).context(JsonIssue::Read)?;
    ensure!(!bytes.is_empty(), JsonIssue::Empty);
    ensure!(bytes.len() as u64 <= MAX_INPUT_BYTES, JsonIssue::TooLarge);
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!(JsonIssue::NotText))
        .context(JsonIssue::NotText)?;
    Ok(LoadedJson { source, text })
}

/// Write beside the destination, then atomically persist. Never replace an input
/// document. The caller owns overwrite confirmation (native save dialog).
pub fn export(source: Option<&Path>, destination: &Path, text: &str) -> Result<()> {
    ensure!(
        destination
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json")),
        JsonIssue::InvalidExtension
    );
    if let (Some(source), Ok(existing)) = (source, destination.canonicalize()) {
        ensure!(
            existing != source.canonicalize().context(JsonIssue::InputMissing)?,
            JsonIssue::SourceOverwrite
        );
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).context(JsonIssue::CreateOutput)?;
    temp.write_all(text.as_bytes())
        .context(JsonIssue::WriteOutput)?;
    temp.as_file().sync_all().context(JsonIssue::WriteOutput)?;
    temp.persist(destination).context(JsonIssue::SaveOutput)?;
    Ok(())
}

fn kind(value: &Val) -> ValueKind {
    match value {
        Val::Obj(_) => ValueKind::Object,
        Val::Arr(_) => ValueKind::Array,
        Val::BStr(_) | Val::TStr(_) => ValueKind::String,
        Val::Num(_) => ValueKind::Number,
        Val::Bool(_) => ValueKind::Boolean,
        Val::Null => ValueKind::Null,
    }
}

/// Every top-level value, so a JSON stream or JSON Lines file works like it does
/// in jq — unless the caller asked for a single document. An empty document is a
/// state, not a parse error.
fn parse(input: &str, mode: Mode) -> Result<Vec<Val>> {
    ensure!(input.len() as u64 <= MAX_INPUT_BYTES, JsonIssue::TooLarge);
    ensure!(!input.trim().is_empty(), JsonIssue::Empty);
    let mut values = Vec::new();
    for value in read::parse_many(input.as_bytes()) {
        values.push(value.map_err(|error| syntax_error(input, &error))?);
    }
    ensure!(!values.is_empty(), JsonIssue::Empty);
    if mode == Mode::Single && values.len() > 1 {
        return Err(anyhow::anyhow!("{} top-level values", values.len()))
            .context(JsonIssue::Multiple);
    }
    Ok(values)
}

/// jaq reports a byte offset; people count lines. `serde_json` rejects every
/// document jaq's superset reader rejects, so re-parsing there is a reliable way
/// to get a position and an expectation, and it only runs once, on failure.
fn syntax_error(input: &str, fallback: &read::Error) -> anyhow::Error {
    let mut stream = serde_json::Deserializer::from_str(input).into_iter::<serde_json::Value>();
    let detail = stream.find_map(|value| value.err());
    match detail {
        Some(error) => anyhow::anyhow!("{error}").context(JsonIssue::Syntax {
            line: error.line(),
            column: error.column(),
        }),
        None => anyhow::anyhow!("{fallback}").context(JsonIssue::Syntax { line: 0, column: 0 }),
    }
}

/// Load and compile a jq program. Only the standard library is available: there
/// is no module path, so a filter can never reach the file system.
fn compile(filter: &str) -> Result<jaq_core::compile::Filter<jaq_core::Native<Data>>> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let arena = Arena::default();
    let modules = Loader::new(defs)
        .load(
            &arena,
            File {
                code: filter,
                path: (),
            },
        )
        .map_err(|errors| anyhow::anyhow!(describe_load(filter, errors)))
        .context(JsonIssue::Filter)?;
    Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| anyhow::anyhow!(describe_compile(errors)))
        .context(JsonIssue::Filter)
}

/// Error slices point into the program text, so their offset is their position.
fn column_of(filter: &str, part: &str) -> Option<usize> {
    let offset = (part.as_ptr() as usize).checked_sub(filter.as_ptr() as usize)?;
    (offset <= filter.len()).then_some(offset + 1)
}

fn at(filter: &str, part: &str, expected: &str) -> String {
    match column_of(filter, part) {
        Some(column) => format!("expected {expected} at column {column}"),
        None => format!("expected {expected}"),
    }
}

fn describe_load(filter: &str, errors: jaq_core::load::Errors<&str, ()>) -> String {
    use jaq_core::load::Error;
    let mut messages = Vec::new();
    for (_file, error) in errors {
        match error {
            Error::Lex(errors) => messages.extend(
                errors
                    .iter()
                    .map(|(expect, part)| at(filter, part, expect.as_str())),
            ),
            Error::Parse(errors) => messages.extend(
                errors
                    .iter()
                    .map(|(expect, part)| at(filter, part, expect.as_str())),
            ),
            Error::Io(errors) => messages.extend(
                errors
                    .iter()
                    .map(|(path, error)| format!("could not load {path}: {error}")),
            ),
        }
    }
    messages.join("; ")
}

fn describe_compile(errors: jaq_core::compile::Errors<&str, ()>) -> String {
    errors
        .iter()
        .flat_map(|(_file, errors)| errors)
        .map(|(symbol, undefined)| format!("undefined {} `{symbol}`", undefined.as_str()))
        .collect::<Vec<_>>()
        .join("; ")
}
