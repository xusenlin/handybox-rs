use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentIssue {
    InputMissing,
    Read,
    NotFile,
    TooLarge,
    Empty,
    Unsupported,
    NeedsOcr,
    Conversion,
    InvalidExtension,
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for DocumentIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InputMissing => "Input file could not be found",
            Self::Read => "Could not read the file",
            Self::NotFile => "Please choose a file, not a folder",
            Self::TooLarge => "File exceeds 64 MiB. Please choose a smaller document",
            Self::Empty => "The file is empty and cannot be converted",
            Self::Unsupported => "Unsupported format. Choose an Office, PDF, EPUB, RTF or CSV document",
            Self::NeedsOcr => "This PDF contains scanned or image-only pages that need OCR. This version supports text-based PDFs only",
            Self::Conversion => "Conversion failed: the file may be damaged, encrypted or contain unsupported content",
            Self::InvalidExtension => "Exported files must use the .md extension",
            Self::SourceOverwrite => "Cannot overwrite the source document. Choose a different path",
            Self::CreateOutput => "Could not create a file in the destination folder",
            Self::WriteOutput => "Could not write Markdown",
            Self::SaveOutput => "Could not save the destination file",
        })
    }
}
impl std::error::Error for DocumentIssue {}

pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const PREVIEW_CHARS: usize = 200_000;
pub const EXTENSIONS: &[&str] = &[
    "doc", "docx", "docm", "odt", "pdf", "ppt", "pps", "pot", "pptx", "pptm", "ppsx", "ppsm",
    "rtf", "epub", "xlsx", "xlsm", "xlsb", "xls", "ods", "odp", "csv",
];

#[derive(Debug)]
pub struct ConvertedDocument {
    pub source: PathBuf,
    pub markdown: String,
    pub format: String,
    pub input_bytes: u64,
    pub elapsed: Duration,
}

impl ConvertedDocument {
    pub fn suggested_name(&self) -> String {
        format!(
            "{}.md",
            self.source
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
        )
    }

    pub fn preview(&self) -> (String, bool) {
        let text: String = self.markdown.chars().take(PREVIEW_CHARS).collect();
        let truncated = text.len() < self.markdown.len();
        (text, truncated)
    }
}

/// Bounded file read; the limit is on compressed input, not parser peak memory.
pub fn convert(path: &Path) -> Result<ConvertedDocument> {
    let started = Instant::now();
    let source = path.canonicalize().context(DocumentIssue::InputMissing)?;
    let file = File::open(&source).context(DocumentIssue::Read)?;
    let meta = file.metadata().context(DocumentIssue::Read)?;
    ensure!(meta.is_file(), DocumentIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, DocumentIssue::TooLarge);
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .context(DocumentIssue::Read)?;
    ensure!(!bytes.is_empty(), DocumentIssue::Empty);
    ensure!(
        bytes.len() as u64 <= MAX_INPUT_BYTES,
        DocumentIssue::TooLarge
    );
    let format = anydoc::Format::from_bytes(&bytes)
        .or_else(|| anydoc::Format::from_path(&source))
        .context(DocumentIssue::Unsupported)?;
    let markdown = match anydoc::to_markdown_bytes(&bytes, format) {
        Ok(text) => text,
        // Keep the engine's message as the cause: it names the pages that need
        // OCR, which is the one detail that makes this error actionable.
        Err(error @ anydoc::ConvertError::NeedsOcr { .. }) => {
            return Err(error).context(DocumentIssue::NeedsOcr);
        }
        Err(error) => return Err(error).context(DocumentIssue::Conversion),
    };
    Ok(ConvertedDocument {
        source,
        markdown,
        format: format!("{format:?}"),
        input_bytes: bytes.len() as u64,
        elapsed: started.elapsed(),
    })
}

/// Write beside the destination, then atomically persist. Never replace the input.
/// The caller owns overwrite confirmation (native save dialog in the desktop).
pub fn export_markdown(source: &Path, destination: &Path, markdown: &str) -> Result<()> {
    ensure!(
        destination
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md")),
        DocumentIssue::InvalidExtension
    );
    if let Ok(existing) = destination.canonicalize() {
        ensure!(
            existing != source.canonicalize().context(DocumentIssue::InputMissing)?,
            DocumentIssue::SourceOverwrite
        );
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).context(DocumentIssue::CreateOutput)?;
    temp.write_all(markdown.as_bytes())
        .context(DocumentIssue::WriteOutput)?;
    temp.as_file()
        .sync_all()
        .context(DocumentIssue::WriteOutput)?;
    temp.persist(destination)
        .context(DocumentIssue::SaveOutput)?;
    Ok(())
}
