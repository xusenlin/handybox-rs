//! A temporary workspace for the things you copy.
//!
//! The system clipboard holds exactly one thing, and the next copy destroys it.
//! This tool keeps what passed through it for as long as the application is
//! running: a list you can look at, copy back from, and throw away. Nothing here
//! is written to disk — closing the window is what empties it.
//!
//! Reading and writing the clipboard is platform I/O and lives in the desktop
//! worker. What lives here is everything that does not need an operating system:
//! what a captured item *is*, when two captures are the same thing, how much
//! room the workspace is allowed to take, and what a line of it reads like.
//!
//! Collecting automatically is the whole reason a tool like this is not just a
//! list of texts, and also the reason it has to be asked for: a clipboard holds
//! passwords and bank details as often as it holds a paragraph. Watching is off
//! until it is switched on, and the switch is the only thing that starts it.
use anyhow::{Context, Result, ensure};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardIssue {
    /// The clipboard holds nothing, or nothing with any content in it.
    Empty,
    /// It holds something, but in a format this workspace does not keep.
    Unsupported,
    TooLarge,
    /// The item was dropped from the workspace before the action reached it.
    Gone,
    InvalidExtension,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for ClipboardIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("There is nothing on the clipboard to collect"),
            Self::Unsupported => {
                f.write_str("The clipboard holds a format this workspace cannot keep")
            }
            Self::TooLarge => {
                f.write_str("This is too large to keep in the workspace. Copy a smaller part of it")
            }
            Self::Gone => f.write_str("That item is no longer in the workspace"),
            Self::InvalidExtension => f.write_str("A picture must be saved with a .png extension"),
            Self::CreateOutput => f.write_str("Could not create a file in the chosen folder"),
            Self::WriteOutput => f.write_str("Could not write the picture"),
            Self::SaveOutput => f.write_str("Could not save to the chosen file"),
        }
    }
}
impl std::error::Error for ClipboardIssue {}

/// How many items the workspace keeps. A session buffer, not an archive: the
/// oldest leaves so the newest can arrive.
pub const MAX_ITEMS: usize = 60;
/// Per text item. Whole files get copied by accident, and the workspace is not
/// where a 40 MiB log belongs.
pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Per picture, as PNG. A screenshot of a large display lands well inside this.
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// Everything together. Images are what make a clipboard history expensive, and
/// this is the ceiling that keeps a session of screenshots from growing forever.
pub const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
/// Longest edge of the thumbnail kept for display. The original is kept too, as
/// PNG, because that is what goes back on the clipboard.
pub const PREVIEW_EDGE: u32 = 480;
/// Characters of a text item shown in the detail panel. Copying back always uses
/// the whole thing; this only bounds what is laid out on screen.
pub const PREVIEW_CHARS: usize = 200_000;
/// What a saved picture is: the PNG already kept, written out unchanged.
pub const PICTURE_EXTENSION: &str = "png";
/// Characters of the one-line preview a row shows.
pub const SUMMARY_CHARS: usize = 120;

pub type ItemId = u64;

/// What a captured item is. The three things a clipboard actually carries
/// between applications; rich text and HTML are kept as their plain text,
/// because that is what pasting them somewhere else is worth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    Files,
}

/// The picture at a size worth drawing, as plain RGBA rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A captured picture. The PNG is the item itself — it is what is put back on
/// the clipboard — and the thumbnail is only what gets drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The picture's own size, not the thumbnail's.
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
    pub thumbnail: Thumbnail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Image(Picture),
    /// Paths, already decoded from whatever references the platform handed over.
    Files(Vec<PathBuf>),
}

impl Content {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Text(_) => Kind::Text,
            Self::Image(_) => Kind::Image,
            Self::Files(_) => Kind::Files,
        }
    }

    /// How big the item itself is — the size anyone would recognize it by, and
    /// the one worth showing. A picture is its PNG, not the thumbnail drawn
    /// beside it, which is an artefact of showing it rather than part of it.
    pub fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image(picture) => picture.png.len(),
            Self::Files(paths) => paths
                .iter()
                .map(|path| path.as_os_str().len() + 1)
                .sum::<usize>(),
        }
    }

    /// What the item costs to keep, thumbnail and all. Only used to decide when
    /// the workspace is full: an uncompressed preview is most of what a
    /// collected screenshot occupies, and a budget that ignored it would be
    /// wrong by an order of magnitude.
    pub fn bytes(&self) -> usize {
        match self {
            Self::Image(picture) => picture.png.len() + picture.thumbnail.rgba.len(),
            other => other.size(),
        }
    }

    /// Whether two captures are the same thing. A hash rather than the content
    /// itself: watching compares every new capture against the whole workspace,
    /// and the pictures in it are megabytes each.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            Self::Text(text) => {
                0u8.hash(&mut hasher);
                text.hash(&mut hasher);
            }
            Self::Image(picture) => {
                1u8.hash(&mut hasher);
                picture.png.hash(&mut hasher);
            }
            Self::Files(paths) => {
                2u8.hash(&mut hasher);
                paths.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// The single line a row shows. Empty when there is nothing worth showing —
    /// a text of pure whitespace — which the caller says in its own words.
    pub fn summary(&self) -> String {
        match self {
            Self::Text(text) => summarize(text),
            // Language-neutral, and exactly what there is to say about a picture
            // before you look at it.
            Self::Image(picture) => format!("{} × {}", picture.width, picture.height),
            Self::Files(paths) => truncate(
                &paths
                    .iter()
                    .map(|path| {
                        path.file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("  ·  "),
            ),
        }
    }

    /// What this item is worth pasting as text, if anything. File references
    /// paste as their paths, which is what a terminal or an editor wants.
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Image(_) => None,
            Self::Files(paths) => Some(
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
}

/// Whether a capture is worth keeping, and small enough to be allowed to.
/// Reading the clipboard is the platform's job; deciding what counts is not.
pub fn accept(content: Content) -> Result<Content> {
    match &content {
        Content::Text(text) => {
            ensure!(!text.is_empty(), ClipboardIssue::Empty);
            ensure!(text.len() <= MAX_TEXT_BYTES, ClipboardIssue::TooLarge);
        }
        Content::Image(picture) => {
            ensure!(!picture.png.is_empty(), ClipboardIssue::Empty);
            ensure!(
                picture.png.len() <= MAX_IMAGE_BYTES,
                ClipboardIssue::TooLarge
            );
        }
        Content::Files(paths) => ensure!(!paths.is_empty(), ClipboardIssue::Empty),
    }
    Ok(content)
}

/// One line standing in for a text. The whole text, not its first line: a first
/// line is often a comment marker, an opening brace or a heading rule, and the
/// row would then say nothing about what was copied. Line breaks and the other
/// control characters become spaces, and a run of whitespace becomes one, so
/// that what survives into the row is the content and not the shape of it.
fn summarize(text: &str) -> String {
    let mut flat = String::with_capacity(text.len().min(SUMMARY_CHARS * 4));
    let mut taken = 0;
    let mut spaced = true;
    for character in text.chars() {
        if character.is_whitespace() || character.is_control() {
            if !spaced {
                flat.push(' ');
                taken += 1;
                spaced = true;
            }
        } else {
            flat.push(character);
            taken += 1;
            spaced = false;
        }
        // Long enough to fill the row; the rest cannot be shown anyway, and a
        // clipboard item may be megabytes of it.
        if taken > SUMMARY_CHARS {
            break;
        }
    }
    truncate(flat.trim_end())
}

/// Bounded by characters rather than by bytes: the cut has to land between two
/// characters, and a row of Chinese is as long as a row of English is wide.
fn truncate(text: &str) -> String {
    match text.char_indices().nth(SUMMARY_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// What a text item adds up to, for the line under it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStats {
    pub lines: usize,
    pub characters: usize,
    pub bytes: usize,
}

pub fn stats(text: &str) -> TextStats {
    TextStats {
        lines: text.lines().count(),
        characters: text.chars().count(),
        bytes: text.len(),
    }
}

/// Write a collected picture to a file. The bytes are the very ones that go
/// back on the clipboard, so what lands on disk is the picture that was copied
/// rather than the preview drawn beside it — and it is written through a
/// temporary file in the destination's own folder and persisted atomically, so
/// a failure halfway leaves no half-written picture behind. The extension is
/// not corrected silently: the native dialog has already asked about
/// overwriting whatever the chosen path names.
pub fn export(png: &[u8], destination: &Path) -> Result<()> {
    ensure!(
        destination
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(PICTURE_EXTENSION)),
        ClipboardIssue::InvalidExtension
    );
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent).context(ClipboardIssue::CreateOutput)?;
    temp.write_all(png).context(ClipboardIssue::WriteOutput)?;
    temp.as_file()
        .sync_all()
        .context(ClipboardIssue::WriteOutput)?;
    temp.persist(destination)
        .context(ClipboardIssue::SaveOutput)?;
    Ok(())
}

/// A platform hands file references over as URIs on one system and as plain
/// paths on another, and a path with a space in it arrives percent-encoded.
/// Anything that is not a `file://` URI is already a path and is left alone.
pub fn path_from_reference(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("file://") else {
        return PathBuf::from(trimmed);
    };
    // A URI keeps the host between the two slashes and the path after it; an
    // empty host is the local machine, which is the only one that can be opened.
    let path = match rest.find('/') {
        Some(at) => &rest[at..],
        None => return PathBuf::from(decode(rest)),
    };
    let decoded = decode(path);
    // file:///C:/Users — the leading slash belongs to the URI, not to the path.
    let windows_drive = decoded.as_bytes().get(2) == Some(&b':')
        && decoded.as_bytes().first().is_some_and(|byte| *byte == b'/')
        && decoded.as_bytes()[1].is_ascii_alphabetic();
    PathBuf::from(if windows_drive {
        decoded[1..].to_owned()
    } else {
        decoded
    })
}

/// Percent-decoding, on the bytes rather than on the characters: a UTF-8 path
/// arrives as one escape per byte, and only all of them together are a name.
fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&raw[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    // Not an escape after all, just a per cent sign in a name.
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: ItemId,
    pub content: Content,
    /// Monotonic, so an item's age is never a system clock change.
    pub captured: Instant,
    fingerprint: u64,
}

impl Item {
    pub fn kind(&self) -> Kind {
        self.content.kind()
    }

    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.captured)
    }
}

/// What a capture did to the workspace. The same thing copied twice is not two
/// items; it is the one item, back at the top where it was just used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    Added(ItemId),
    Moved(ItemId),
}

impl Recorded {
    pub fn id(self) -> ItemId {
        match self {
            Self::Added(id) | Self::Moved(id) => id,
        }
    }
}

/// The session's items, newest first. In memory only, for as long as the
/// application is running.
#[derive(Debug, Default)]
pub struct History {
    items: Vec<Item>,
    next: ItemId,
    bytes: usize,
}

impl History {
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// What the workspace is holding, by the same measure the limit uses.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// What the items add up to as things rather than as memory, so that the
    /// total under the list is the sum of the sizes written beside the rows.
    pub fn size(&self) -> usize {
        self.items.iter().map(|item| item.content.size()).sum()
    }

    pub fn get(&self, id: ItemId) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Take a capture. Content already in the workspace is moved to the top
    /// instead of being kept twice — including the capture that follows putting
    /// an item back on the clipboard, which is the same thing arriving again.
    pub fn record(&mut self, content: Content, at: Instant) -> Recorded {
        let fingerprint = content.fingerprint();
        if let Some(position) = self
            .items
            .iter()
            .position(|item| item.fingerprint == fingerprint)
        {
            let mut item = self.items.remove(position);
            item.captured = at;
            let id = item.id;
            self.items.insert(0, item);
            return Recorded::Moved(id);
        }
        self.next += 1;
        let id = self.next;
        self.bytes += content.bytes();
        self.items.insert(
            0,
            Item {
                id,
                content,
                captured: at,
                fingerprint,
            },
        );
        self.evict();
        Recorded::Added(id)
    }

    pub fn remove(&mut self, id: ItemId) -> bool {
        let Some(position) = self.items.iter().position(|item| item.id == id) else {
            return false;
        };
        let item = self.items.remove(position);
        self.bytes = self.bytes.saturating_sub(item.content.bytes());
        true
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.bytes = 0;
    }

    /// Make room from the far end. The item that was just captured is the one
    /// thing that always stays: a picture larger than the whole budget still
    /// belongs on screen, on its own, rather than being dropped on arrival.
    fn evict(&mut self) {
        while self.items.len() > MAX_ITEMS || (self.bytes > MAX_TOTAL_BYTES && self.items.len() > 1)
        {
            let Some(item) = self.items.pop() else { break };
            self.bytes = self.bytes.saturating_sub(item.content.bytes());
        }
    }
}
