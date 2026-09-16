//! Look inside an archive, take out what is in it, and put a folder into one.
//!
//! Reading an archive is two steps, like every other tool here that writes
//! files: listing only reads the table of contents, and extraction happens
//! afterwards, from a selection, into a folder someone chose. A listing costs
//! the header rather than the content, so a 4 GiB archive opens as fast as an
//! empty one.
//!
//! **A name inside an archive is not a path.** It is a string somebody else
//! wrote, and the whole risk of unpacking anything is that the string says
//! `../../.ssh/authorized_keys`. Every name is checked here rather than by the
//! page: an absolute name, a `..` component, a drive letter or a link is
//! refused outright, and what would be written is checked again — after the
//! folders exist — to be genuinely inside the folder that was chosen. Refused
//! entries are listed as refused instead of being quietly renamed into
//! something safe, because a file that lands somewhere other than where the
//! archive said is its own kind of surprise.
//!
//! Nothing is ever replaced. An extracted name that is already taken is skipped
//! and counted, which is what makes extracting twice harmless.
//!
//! Size is a limit, not a hope: an archive that declares more than
//! [`MAX_TOTAL_BYTES`] is refused before a byte is written, and the writing
//! itself stops at the same budget — a header can lie about how much is inside,
//! and that lie is the whole of a decompression bomb.
//!
//! Passwords are out of scope in both directions. An encrypted archive is
//! reported as encrypted rather than half-opened, and nothing this tool writes
//! is encrypted; the Hash & encrypt tool is where a file gets a passphrase.
use anyhow::{Context, Result, anyhow, bail, ensure};
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, Password};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveIssue {
    InputMissing,
    Read,
    NotFile,
    /// A file was chosen where a folder is needed: creating an archive packs a
    /// folder.
    NotFolder,
    TooLarge,
    /// The bytes are not a ZIP or a 7z archive.
    NotArchive,
    /// The archive, or the part of it that was asked for, is behind a password.
    /// This tool does not ask for one.
    Encrypted,
    /// A codec this build does not include. Named separately from a damaged
    /// file because there is nothing wrong with the archive.
    Unsupported,
    /// The archive is not readable as one: truncated, or not what it says.
    Damaged,
    /// A folder with nothing in it to pack, or an archive with nothing in it.
    Empty,
    NothingSelected,
    /// Unpacking this would write more than one operation is allowed to.
    Expanded,
    InvalidExtension,
    /// The archive would be written into the folder it is packing, or over the
    /// archive being read.
    SourceOverwrite,
    CreateOutput,
    WriteOutput,
    SaveOutput,
}

impl std::fmt::Display for ArchiveIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("Input file could not be found"),
            Self::Read => f.write_str("Could not read the file"),
            Self::NotFile => f.write_str("Please choose a file, not a folder"),
            Self::NotFolder => f.write_str("Please choose a folder, not a file"),
            Self::TooLarge => {
                f.write_str("Archives are limited to 2 GiB. Please choose a smaller one")
            }
            Self::NotArchive => f.write_str("This file is not a ZIP or 7z archive"),
            Self::Encrypted => {
                f.write_str("This archive is password protected, which this tool cannot open")
            }
            Self::Unsupported => {
                f.write_str("This archive uses a compression method this build cannot read")
            }
            Self::Damaged => f.write_str("This archive is damaged or incomplete"),
            Self::Empty => f.write_str("There is nothing here to pack"),
            Self::NothingSelected => f.write_str("Nothing is selected"),
            Self::Expanded => {
                f.write_str("Unpacking this would write more than 16 GiB. Extract it in parts")
            }
            Self::InvalidExtension => {
                f.write_str("The file name must end with the chosen format's extension")
            }
            Self::SourceOverwrite => {
                f.write_str("Cannot write the archive into the folder it is packing")
            }
            Self::CreateOutput => f.write_str("Could not create a file in the chosen folder"),
            Self::WriteOutput => f.write_str("Could not write the archive"),
            Self::SaveOutput => f.write_str("Could not save to the chosen file"),
        }
    }
}
impl std::error::Error for ArchiveIssue {}

/// An archive file this tool will open. Past this the table of contents alone
/// stops being something to hold in memory.
pub const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Entries one list holds. An archive may contain more; the list says so rather
/// than growing until the page cannot be drawn.
pub const MAX_ENTRIES: usize = 20_000;
/// Bytes one extraction writes, or one archive is packed from. This is what
/// stands between a 40 KiB file and the 4 TiB it claims to hold.
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Deeper than any folder worth packing, and shallow enough to recurse safely.
pub const MAX_DEPTH: usize = 64;
/// Offered in the open dialog, and what this tool can read.
pub const EXTENSIONS: &[&str] = &["zip", "7z"];

/// Whether a path names an archive this tool can open, by its extension alone.
/// What is actually opened is decided by the bytes; this only routes a drop.
pub fn claims(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The two formats. ZIP is what everything can open; 7z is what makes a folder
/// of source or text noticeably smaller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Zip,
    SevenZ,
}

impl Kind {
    /// Printed on specifications rather than translated.
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::SevenZ => "7z",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::SevenZ => "7z",
        }
    }

    fn accepts(self, extension: &str) -> bool {
        extension.eq_ignore_ascii_case(self.extension())
    }
}

/// One entry of an archive, as the archive describes it. Nothing here has been
/// read or unpacked: this is the table of contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The name the archive records, separators and all, decoded the way the
    /// file meant it — see [`zip_name`] for what that costs in a ZIP. Shown as
    /// it is, and written to disk as it is.
    pub name: String,
    /// Where it sits in the archive. A ZIP is unpacked by this rather than by
    /// name, because two entries may record the same name and only one of them
    /// would ever be found by it.
    pub index: usize,
    pub size: u64,
    /// What it takes up inside the archive. Zero for an entry of a solid 7z
    /// block, where the packed bytes belong to the block rather than the file.
    pub packed: u64,
    pub directory: bool,
    /// What the archive recorded, in the archive's own reckoning of time. None
    /// when it recorded nothing.
    pub modified: Option<String>,
    pub encrypted: bool,
    /// Whether the archive recorded it as a program. Only this much of a mode
    /// is carried back out; see [`apply_mode`].
    pub executable: bool,
    /// Whether this tool would write it: a name that escapes the folder, or an
    /// entry that is a link, is listed and refused rather than unpacked.
    pub safe: bool,
}

/// What an archive holds.
#[derive(Clone, Debug)]
pub struct Listing {
    pub source: PathBuf,
    pub kind: Kind,
    /// By name, the way a file manager shows them.
    pub entries: Vec<Entry>,
    pub files: usize,
    pub folders: usize,
    /// Everything unpacked, as the archive declares it.
    pub size: u64,
    /// The archive's own size on disk.
    pub packed: u64,
    /// Entries beyond the cap, which are in the archive but not in the list.
    pub left_out: usize,
    /// Entries this tool would refuse to write. Zero for every archive anyone
    /// made on purpose.
    pub refused: usize,
    pub encrypted: bool,
}

impl Listing {
    /// How much smaller the archive is than what is in it.
    pub fn ratio(&self) -> f64 {
        if self.size == 0 {
            return 0.0;
        }
        1.0 - (self.packed as f64 / self.size as f64)
    }
}

/// What an extraction did. Skipped and refused entries are not failures: one is
/// a name that was already there, the other a name this tool will not write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extraction {
    pub into: PathBuf,
    pub files: usize,
    pub folders: usize,
    pub bytes: u64,
    /// A name that was already taken in the destination folder.
    pub skipped: usize,
    /// The first of those names. A count on its own reads as "it did not
    /// work": the archive's own path is what says *where* the file already is,
    /// which is usually a folder of the same name sitting beside the archive.
    pub first_skipped: Option<String>,
    /// A name that would have landed outside the folder, or a link.
    pub refused: usize,
    pub failed: usize,
    pub cancelled: bool,
    /// The archive held more than it declared, and writing stopped at the
    /// budget. What was written is complete; what follows it was not written.
    pub over_budget: bool,
    pub elapsed: Duration,
}

/// One file that would go into a new archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Its name inside the archive: the chosen folder's own name, then the path
    /// under it, with `/` separators.
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    /// A folder with nothing under it. Recorded because it is the one thing
    /// that would otherwise be lost: every other folder is implied by a name.
    pub directory: bool,
}

/// What a folder would be packed as.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub root: PathBuf,
    pub items: Vec<Item>,
    pub size: u64,
    pub left_out: usize,
}

/// What was written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Creation {
    pub path: PathBuf,
    pub kind: Kind,
    pub files: usize,
    /// What went in, before compression.
    pub size: u64,
    /// The archive itself.
    pub packed: u64,
    pub failed: usize,
    pub cancelled: bool,
    pub elapsed: Duration,
}

/// Which format a file actually is, by its first bytes. The extension is only
/// ever a hint: a `.zip` that is really a 7z opens as a 7z here.
pub fn format(path: &Path) -> Result<Kind> {
    let mut head = [0u8; 6];
    let read = File::open(path)
        .context(ArchiveIssue::Read)?
        .read(&mut head)
        .context(ArchiveIssue::Read)?;
    let head = &head[..read];
    if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(Kind::SevenZ);
    }
    // "PK" and one of the record signatures: a local file, a central directory
    // of an empty archive, or a spanned marker.
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Ok(Kind::Zip);
    }
    bail!(ArchiveIssue::NotArchive)
}

/// Read an archive's table of contents. Reads only: nothing here unpacks,
/// writes or changes anything.
pub fn list(path: &Path) -> Result<Listing> {
    let source = path.canonicalize().context(ArchiveIssue::InputMissing)?;
    let meta = fs::metadata(&source).context(ArchiveIssue::Read)?;
    ensure!(meta.is_file(), ArchiveIssue::NotFile);
    ensure!(meta.len() <= MAX_INPUT_BYTES, ArchiveIssue::TooLarge);
    let kind = format(&source)?;
    let mut entries = match kind {
        Kind::Zip => zip_entries(&source)?,
        Kind::SevenZ => sevenz_entries(&source)?,
    };
    // By name, case aside: an archive listed differently from the file manager
    // beside it is an archive nobody can find anything in.
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    let left_out = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);
    let folders = entries.iter().filter(|entry| entry.directory).count();
    Ok(Listing {
        source,
        kind,
        files: entries.len() - folders,
        folders,
        size: entries.iter().map(|entry| entry.size).sum(),
        packed: meta.len(),
        refused: entries.iter().filter(|entry| !entry.safe).count(),
        encrypted: entries.iter().any(|entry| entry.encrypted),
        left_out,
        entries,
    })
}

/// Unpack the named entries into a folder.
///
/// The names come from a listing rather than indices, because a 7z archive is
/// read in the order its blocks were packed, not the order it is shown in. An
/// archive that records the same name twice hands both copies to the same
/// destination, where the second one is skipped as a name already taken.
pub fn extract(
    source: &Path,
    into: &Path,
    wanted: &HashSet<String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
) -> Result<Extraction> {
    let started = Instant::now();
    ensure!(!wanted.is_empty(), ArchiveIssue::NothingSelected);
    let source = source.canonicalize().context(ArchiveIssue::InputMissing)?;
    let into = into.canonicalize().context(ArchiveIssue::InputMissing)?;
    ensure!(
        fs::metadata(&into).context(ArchiveIssue::Read)?.is_dir(),
        ArchiveIssue::NotFolder
    );
    let kind = format(&source)?;
    let mut extraction = Extraction {
        into: into.clone(),
        ..Default::default()
    };
    let mut budget = MAX_TOTAL_BYTES;
    match kind {
        Kind::Zip => zip_extract(
            &source,
            &into,
            wanted,
            cancel,
            progress,
            &mut budget,
            &mut extraction,
        )?,
        Kind::SevenZ => sevenz_extract(
            &source,
            &into,
            wanted,
            cancel,
            progress,
            &mut budget,
            &mut extraction,
        )?,
    }
    extraction.elapsed = started.elapsed();
    Ok(extraction)
}

/// Every file in a folder and under it, with the name it would carry inside an
/// archive. Reads names and sizes only.
///
/// The folder's own name is the first part of every entry, so unpacking the
/// result gives back one folder rather than scattering its contents into
/// whatever folder someone happened to extract into.
pub fn survey(folder: &Path) -> Result<Plan> {
    let root = folder.canonicalize().context(ArchiveIssue::InputMissing)?;
    ensure!(
        fs::metadata(&root).context(ArchiveIssue::Read)?.is_dir(),
        ArchiveIssue::NotFolder
    );
    let top = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    let mut plan = Plan {
        root,
        ..Default::default()
    };
    walk(&plan.root.clone(), &top, 0, &mut plan);
    ensure!(!plan.items.is_empty(), ArchiveIssue::Empty);
    plan.items.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    plan.size = plan.items.iter().map(|item| item.size).sum();
    Ok(plan)
}

/// Pack the named files into a new archive.
///
/// Written through a temporary file in the destination's own folder and moved
/// into place at the end, so an interrupted run leaves no half-written archive
/// where a real one is expected. The native save dialog owns the question of
/// replacing an existing file; what is refused here is writing the archive into
/// the folder being packed, which would ask it to contain itself.
pub fn create(
    plan: &Plan,
    wanted: &HashSet<String>,
    destination: &Path,
    kind: Kind,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
) -> Result<Creation> {
    let started = Instant::now();
    ensure!(!wanted.is_empty(), ArchiveIssue::NothingSelected);
    ensure!(
        destination
            .extension()
            .is_some_and(|extension| kind.accepts(&extension.to_string_lossy())),
        ArchiveIssue::InvalidExtension
    );
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(
        !parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_owned())
            .starts_with(&plan.root),
        ArchiveIssue::SourceOverwrite
    );
    let items: Vec<&Item> = plan
        .items
        .iter()
        .filter(|item| wanted.contains(&item.name))
        .collect();
    ensure!(!items.is_empty(), ArchiveIssue::NothingSelected);
    ensure!(
        items.iter().map(|item| item.size).sum::<u64>() <= MAX_TOTAL_BYTES,
        ArchiveIssue::Expanded
    );

    let mut temp = tempfile::NamedTempFile::new_in(parent).context(ArchiveIssue::CreateOutput)?;
    let mut creation = Creation {
        kind,
        ..Default::default()
    };
    match kind {
        Kind::Zip => zip_create(&items, temp.as_file_mut(), cancel, progress, &mut creation)?,
        Kind::SevenZ => sevenz_create(&items, temp.as_file_mut(), cancel, progress, &mut creation)?,
    }
    temp.as_file()
        .sync_all()
        .context(ArchiveIssue::WriteOutput)?;
    creation.packed = temp
        .as_file()
        .metadata()
        .map(|meta| meta.len())
        .unwrap_or_default();
    temp.persist(destination)
        .context(ArchiveIssue::SaveOutput)?;
    creation.path = destination.to_owned();
    creation.elapsed = started.elapsed();
    Ok(creation)
}

/// The name a new archive would suggest: the folder's own, with the chosen
/// format's extension.
pub fn suggested_name(folder: &Path, kind: Kind) -> String {
    let stem = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    format!("{stem}.{}", kind.extension())
}

/// Where an entry may be written under `into`, or `None` when the name in the
/// archive is not one this tool will write.
///
/// Refused rather than repaired: a leading `/`, a `..` anywhere, a drive
/// letter, or a name that turns out to be nothing at all. Trimming those into
/// something writable is what turns an archive that tried to escape into an
/// archive that quietly landed somewhere unexpected.
pub fn destination(into: &Path, name: &str) -> Option<PathBuf> {
    // The format says `/`. Archives written on Windows sometimes say `\`, and a
    // backslash inside a name is a separator there rather than a character.
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return None;
    }
    let mut path = into.to_path_buf();
    let mut parts = 0;
    for part in name.split('/') {
        match part {
            // A trailing separator is how a folder entry is written.
            "" | "." => continue,
            ".." => return None,
            // A drive letter is not a relative name, and a NUL is not a name.
            part if part.contains(':') || part.contains('\0') => return None,
            part => {
                path.push(part);
                parts += 1;
            }
        }
    }
    (parts > 0).then_some(path)
}

/// One entry, on disk. Every counter an extraction reports is moved here, so
/// both formats agree about what skipped, refused and failed mean.
fn place(
    into: &Path,
    entry: &Entry,
    reader: &mut dyn Read,
    budget: &mut u64,
    extraction: &mut Extraction,
) {
    let Some(target) = destination(into, &entry.name).filter(|_| entry.safe) else {
        extraction.refused += 1;
        return;
    };
    if entry.directory {
        match fs::create_dir_all(&target) {
            Ok(()) => extraction.folders += 1,
            Err(_) => extraction.failed += 1,
        }
        return;
    }
    let Some(parent) = target.parent() else {
        extraction.failed += 1;
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        extraction.failed += 1;
        return;
    }
    // The folders exist now, so this is the moment the destination can be
    // resolved for real. A link somewhere above it — put there by an earlier
    // entry, or already on disk — is the one way a checked name still lands
    // outside the folder that was chosen.
    match parent.canonicalize() {
        Ok(resolved) if resolved.starts_with(into) => {}
        _ => {
            extraction.refused += 1;
            return;
        }
    }
    // Never replace: extracting the same archive twice leaves the first result
    // alone and says how many names were already taken.
    let mut file = match File::create_new(&target) {
        Ok(file) => BufWriter::new(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            extraction.skipped += 1;
            extraction
                .first_skipped
                .get_or_insert_with(|| entry.name.clone());
            return;
        }
        Err(_) => {
            extraction.failed += 1;
            return;
        }
    };
    // A header that declares a small file and then holds a large one is the
    // whole of a decompression bomb, so what is left of the budget — not what
    // the entry claims — is what may be written.
    let written = io::copy(&mut reader.take(*budget), &mut file).and_then(|written| {
        file.flush()?;
        Ok(written)
    });
    match written {
        Ok(written) if written >= *budget => {
            // A file cut off at the budget is not the file the archive holds.
            let _ = fs::remove_file(&target);
            *budget = 0;
            extraction.over_budget = true;
        }
        Ok(written) => {
            *budget -= written;
            extraction.bytes += written;
            extraction.files += 1;
            apply_mode(&target, entry);
        }
        Err(_) => {
            let _ = fs::remove_file(&target);
            extraction.failed += 1;
        }
    }
}

/// Keep a program a program. Only the executable bits travel, and only as
/// `755`: the rest of a recorded mode is someone else's umask, and a set-user-id
/// bit out of an archive is not something to restore on trust.
#[cfg(unix)]
fn apply_mode(target: &Path, entry: &Entry) {
    use std::os::unix::fs::PermissionsExt;
    if entry.executable {
        let _ = fs::set_permissions(target, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(not(unix))]
fn apply_mode(_target: &Path, _entry: &Entry) {}

/// What a ZIP entry is actually called.
///
/// A ZIP stores a name as bytes and a single flag saying whether they are
/// UTF-8. Half the world's archivers never set that flag and write UTF-8
/// anyway, and the format's own answer for the unflagged case — CP437, the
/// code page of MS-DOS — turns `使用说明.md` into `Σ╜┐τö¿Φ»┤µÿÄ.md`. That is not
/// a display problem: this name is what gets written to disk, so a file
/// extracted under it is a file nobody can find.
///
/// So the bytes decide, in the order that is actually right more often:
///
/// 1. Valid UTF-8 is UTF-8, flag or no flag. This is every archive made on
///    macOS or Linux in the last fifteen years, and the reported bug.
/// 2. Otherwise the name predates UTF-8 and is in some legacy code page, with
///    nothing in the file to say which. GBK is the guess this build makes,
///    because it ships in Chinese and English and is the encoding a Windows
///    archiver in that half of the world writes. It is a guess; it is also the
///    one that leaves a Chinese name readable instead of certainly mangling it.
/// 3. `zip`'s own reading is kept when GBK cannot make sense of the bytes.
fn zip_name(raw: &[u8], decoded: &str) -> String {
    if let Ok(utf8) = std::str::from_utf8(raw) {
        return utf8.to_owned();
    }
    let (name, _, broken) = encoding_rs::GBK.decode(raw);
    if broken {
        return decoded.to_owned();
    }
    name.into_owned()
}

/// The table of contents of a ZIP. `by_index_raw` is deliberate: it reads the
/// entry's description without starting to decrypt or decompress it, which is
/// what makes listing a password-protected archive possible at all.
fn zip_entries(source: &Path) -> Result<Vec<Entry>> {
    let file = BufReader::new(File::open(source).context(ArchiveIssue::Read)?);
    let mut archive = ZipArchive::new(file).map_err(zip_issue)?;
    let mut entries = Vec::with_capacity(archive.len().min(MAX_ENTRIES));
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(zip_issue)?;
        let name = zip_name(entry.name_raw(), entry.name());
        // A link is a name pointing somewhere else, which is exactly the thing
        // a checked path cannot check. Listed, and never written.
        let link = entry.is_symlink();
        entries.push(Entry {
            index,
            safe: !link && destination(Path::new("/"), &name).is_some(),
            size: entry.size(),
            packed: entry.compressed_size(),
            directory: entry.is_dir(),
            modified: entry.last_modified().filter(|at| at.is_valid()).map(|at| {
                stamp(
                    i64::from(at.year()),
                    at.month().into(),
                    at.day().into(),
                    at.hour().into(),
                    at.minute().into(),
                )
            }),
            encrypted: entry.encrypted(),
            executable: entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0),
            name,
        });
    }
    Ok(entries)
}

fn zip_extract(
    source: &Path,
    into: &Path,
    wanted: &HashSet<String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
    budget: &mut u64,
    extraction: &mut Extraction,
) -> Result<()> {
    let listed = zip_entries(source)?;
    let chosen: Vec<&Entry> = listed
        .iter()
        .filter(|entry| wanted.contains(&entry.name))
        .collect();
    ensure!(!chosen.is_empty(), ArchiveIssue::NothingSelected);
    // Refused before a byte is written, from what the archive declares. The
    // budget below is the same limit applied to what it actually holds.
    ensure!(
        chosen.iter().map(|entry| entry.size).sum::<u64>() <= MAX_TOTAL_BYTES,
        ArchiveIssue::Expanded
    );
    ensure!(
        !chosen.iter().any(|entry| entry.encrypted),
        ArchiveIssue::Encrypted
    );
    let file = BufReader::new(File::open(source).context(ArchiveIssue::Read)?);
    let mut archive = ZipArchive::new(file).map_err(zip_issue)?;
    let mut done = 0;
    for entry in chosen {
        if cancel.load(Ordering::Relaxed) || *budget == 0 {
            extraction.cancelled = cancel.load(Ordering::Relaxed);
            break;
        }
        progress(done);
        done += 1;
        // A folder entry has no stream to read, and neither has a refused one.
        if entry.directory || !entry.safe {
            place(into, entry, &mut io::empty(), budget, extraction);
            continue;
        }
        // By index, not by name. The name shown is this tool's own reading of
        // the bytes in the header, and two entries may record the same name
        // anyway — only one of them would ever be found by it.
        match archive.by_index(entry.index) {
            Ok(mut stream) => place(into, entry, &mut stream, budget, extraction),
            // One unreadable entry is not a failed extraction: the rest of the
            // archive is still there to take out.
            Err(_) => extraction.failed += 1,
        }
    }
    progress(done);
    Ok(())
}

/// The table of contents of a 7z. Only the header is read; the blocks stay
/// where they are.
fn sevenz_entries(source: &Path) -> Result<Vec<Entry>> {
    let archive = sevenz_rust2::Archive::open(source).map_err(sevenz_issue)?;
    Ok(archive
        .files
        .iter()
        .enumerate()
        // A 7z records its names as UTF-16, so there is nothing to guess at:
        // what the header says is what the name is.
        .map(|(index, entry)| Entry {
            index,
            safe: destination(Path::new("/"), &entry.name).is_some(),
            name: entry.name.clone(),
            size: entry.size,
            packed: entry.compressed_size,
            directory: entry.is_directory,
            modified: entry
                .has_last_modified_date
                .then(|| unix_seconds(u64::from(entry.last_modified_date)))
                .flatten()
                .map(|seconds| {
                    let (year, month, day, hour, minute, _) = civil(seconds);
                    stamp(year, month, day, hour, minute)
                }),
            // Header encryption is refused when the archive is opened; an
            // encrypted block inside a readable header is found on the way out.
            encrypted: false,
            // 7z records Windows attributes. The Unix mode is in the high half
            // of them, and only when the archive was written on Unix at all.
            executable: entry.has_windows_attributes
                && entry.windows_attributes & 0x8000 != 0
                && (entry.windows_attributes >> 16) & 0o111 != 0,
        })
        .collect())
}

/// Stopping in the middle of a 7z. The reader walks whole blocks and only takes
/// a `false` from the closure as the end of the block it is in, so the way to
/// put a stop to the entire archive is an error it hands straight back.
const CANCELLED: &str = "handybox: stopped";

fn sevenz_extract(
    source: &Path,
    into: &Path,
    wanted: &HashSet<String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
    budget: &mut u64,
    extraction: &mut Extraction,
) -> Result<()> {
    let listed = sevenz_entries(source)?;
    let chosen: Vec<&Entry> = listed
        .iter()
        .filter(|entry| wanted.contains(&entry.name))
        .collect();
    ensure!(!chosen.is_empty(), ArchiveIssue::NothingSelected);
    ensure!(
        chosen.iter().map(|entry| entry.size).sum::<u64>() <= MAX_TOTAL_BYTES,
        ArchiveIssue::Expanded
    );
    let mut reader =
        sevenz_rust2::ArchiveReader::open(source, Password::empty()).map_err(sevenz_issue)?;
    let mut done = 0;
    let outcome = reader.for_each_entries(|entry, stream| {
        if cancel.load(Ordering::Relaxed) {
            extraction.cancelled = true;
            return Err(sevenz_rust2::Error::Other(CANCELLED.into()));
        }
        if *budget == 0 {
            return Err(sevenz_rust2::Error::Other(CANCELLED.into()));
        }
        let Some(listed) = chosen.iter().find(|listed| listed.name == entry.name) else {
            // A block is one stream: an entry that is not wanted still has to
            // be read past, or the entry after it starts mid-file.
            io::copy(stream, &mut io::sink()).ok();
            return Ok(true);
        };
        progress(done);
        done += 1;
        place(into, listed, stream, budget, extraction);
        Ok(true)
    });
    progress(done);
    match outcome {
        Ok(()) => Ok(()),
        // Our own stop, handed back as it was given.
        Err(sevenz_rust2::Error::Other(message)) if message == CANCELLED => Ok(()),
        Err(error) => Err(sevenz_issue(error)),
    }
}

fn zip_create(
    items: &[&Item],
    file: &mut File,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
    creation: &mut Creation,
) -> Result<()> {
    let mut writer = ZipWriter::new(BufWriter::new(file));
    for (done, item) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            creation.cancelled = true;
            break;
        }
        progress(done);
        let options = options(item);
        let written = if item.directory {
            writer
                .add_directory(item.name.clone(), options)
                .map(|()| 0)
                .map_err(anyhow::Error::from)
        } else {
            writer
                .start_file(item.name.clone(), options)
                .map_err(anyhow::Error::from)
                .and_then(|()| {
                    let mut source = BufReader::new(File::open(&item.path)?);
                    Ok(io::copy(&mut source, &mut writer)?)
                })
        };
        match written {
            // A file that vanished or turned unreadable between the survey and
            // now is one file missing from the archive, not a failed archive.
            Err(_) => creation.failed += 1,
            Ok(_) if item.directory => {}
            Ok(written) => {
                creation.files += 1;
                creation.size += written;
            }
        }
    }
    progress(items.len());
    writer.finish().context(ArchiveIssue::WriteOutput)?;
    Ok(())
}

/// What a ZIP entry is written with. Deflate is what every unpacker on every
/// platform reads; on Unix the executable bit travels with the entry, because
/// an archived program that comes out unable to run is a broken archive.
fn options(item: &Item) -> SimpleFileOptions {
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        // Past 4 GiB an entry needs the 64-bit record, and a stream cannot be
        // told to grow one afterwards.
        .large_file(item.size >= u32::MAX as u64);
    // The default is the day the format was born. A file keeps the time it was
    // last written instead.
    let options = match zip_time(&item.path) {
        Some(at) => options.last_modified_time(at),
        None => options,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&item.path)
            .map(|meta| meta.permissions().mode())
            .unwrap_or(0o644);
        options.unix_permissions(if mode & 0o111 != 0 { 0o755 } else { 0o644 })
    }
    #[cfg(not(unix))]
    options
}

fn sevenz_create(
    items: &[&Item],
    file: &mut File,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize),
    creation: &mut Creation,
) -> Result<()> {
    let mut writer = ArchiveWriter::new(file).context(ArchiveIssue::WriteOutput)?;
    for (done, item) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            creation.cancelled = true;
            break;
        }
        progress(done);
        let entry = ArchiveEntry::from_path(&item.path, item.name.clone());
        let source = (!item.directory)
            .then(|| File::open(&item.path))
            .transpose();
        let written = match source {
            Ok(source) => writer
                .push_archive_entry(entry, source.map(BufReader::new))
                .map(|_| ())
                .map_err(anyhow::Error::from),
            Err(error) => Err(anyhow::Error::from(error)),
        };
        match written {
            Err(_) => creation.failed += 1,
            Ok(()) if item.directory => {}
            Ok(()) => {
                creation.files += 1;
                creation.size += item.size;
            }
        }
    }
    progress(items.len());
    writer.finish().context(ArchiveIssue::WriteOutput)?;
    Ok(())
}

/// Walk a folder, naming every file the way the archive will.
fn walk(dir: &Path, prefix: &str, depth: usize, plan: &mut Plan) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(listing) = fs::read_dir(dir) else {
        return;
    };
    let mut empty = true;
    for entry in listing.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Links are never followed: what a link points at belongs to someone
        // else, and following one into a parent folder would pack the world.
        if kind.is_symlink() {
            continue;
        }
        empty = false;
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if kind.is_dir() {
            walk(&entry.path(), &name, depth + 1, plan);
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if plan.items.len() >= MAX_ENTRIES {
            plan.left_out += 1;
            continue;
        }
        plan.items.push(Item {
            name,
            path: entry.path(),
            size: meta.len(),
            directory: false,
        });
    }
    // A folder with nothing in it is the one thing a list of files cannot say.
    // Every other folder is implied by the names under it.
    if empty && depth > 0 && plan.items.len() < MAX_ENTRIES {
        plan.items.push(Item {
            name: format!("{prefix}/"),
            path: dir.to_owned(),
            size: 0,
            directory: true,
        });
    }
}

/// What a ZIP failure actually was. The crate says it in a sentence; the page
/// needs the category.
fn zip_issue(error: zip::result::ZipError) -> anyhow::Error {
    let issue = match &error {
        zip::result::ZipError::UnsupportedArchive(message)
            if message.contains("Password") || message.contains("password") =>
        {
            ArchiveIssue::Encrypted
        }
        zip::result::ZipError::UnsupportedArchive(_) => ArchiveIssue::Unsupported,
        zip::result::ZipError::InvalidArchive(_) => ArchiveIssue::Damaged,
        zip::result::ZipError::Io(_) => ArchiveIssue::Read,
        _ => ArchiveIssue::Damaged,
    };
    anyhow!(error.to_string()).context(issue)
}

fn sevenz_issue(error: sevenz_rust2::Error) -> anyhow::Error {
    let issue = match &error {
        sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_) => {
            ArchiveIssue::Encrypted
        }
        sevenz_rust2::Error::UnsupportedCompressionMethod(_)
        | sevenz_rust2::Error::Unsupported(_)
        | sevenz_rust2::Error::ExternalUnsupported
        | sevenz_rust2::Error::UnsupportedVersion { .. } => ArchiveIssue::Unsupported,
        sevenz_rust2::Error::BadSignature(_) => ArchiveIssue::NotArchive,
        sevenz_rust2::Error::Io(_, _) | sevenz_rust2::Error::FileOpen(_, _) => ArchiveIssue::Read,
        _ => ArchiveIssue::Damaged,
    };
    anyhow!(error.to_string()).context(issue)
}

/// Windows file time — 100-nanosecond ticks since 1601 — as seconds since 1970.
/// None for the times before that, which only appear in a file that recorded
/// nothing at all.
fn unix_seconds(ticks: u64) -> Option<i64> {
    const PER_SECOND: u64 = 10_000_000;
    const TO_UNIX: i64 = 11_644_473_600;
    let seconds = i64::try_from(ticks / PER_SECOND).ok()? - TO_UNIX;
    (seconds > 0).then_some(seconds)
}

/// Seconds since 1970 as the parts of a date: year, month, day, hour, minute,
/// second. No zone is applied in either direction — a ZIP records local time
/// with no zone and a 7z records UTC, and converting either would need a
/// timezone database this build does not carry.
fn civil(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Days to a calendar date, counting from March so that the leap day is the
    // last day of the year rather than a hole in the middle of it.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        month as u32,
        day as u32,
        (rest / 3600) as u32,
        ((rest % 3600) / 60) as u32,
        (rest % 60) as u32,
    )
}

/// The date a listing shows. To the minute: the oldest of these formats records
/// seconds in steps of two, and nobody reads an archive by the second.
fn stamp(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// When a file was last written, as a ZIP records it.
///
/// Written as UTC. A ZIP's date field carries local time with no zone at all,
/// so the only alternative would be to guess one; the archives this writes are
/// therefore consistent rather than locally correct, and any unpacker shows the
/// same time this one does.
fn zip_time(path: &Path) -> Option<zip::DateTime> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (year, month, day, hour, minute, second) = civil(i64::try_from(seconds).ok()?);
    zip::DateTime::from_date_and_time(
        u16::try_from(year).ok()?,
        month as u8,
        day as u8,
        hour as u8,
        minute as u8,
        second as u8,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            index: 0,
            size: 0,
            packed: 0,
            directory: false,
            modified: None,
            encrypted: false,
            executable: false,
            safe: true,
        }
    }

    /// The budget is what a header cannot talk its way past: an entry that
    /// declared nothing and holds more than is left writes nothing at all.
    #[test]
    fn an_entry_that_outgrows_the_budget_leaves_no_file_behind() {
        let directory = tempfile::tempdir().unwrap();
        // Resolved, the way `extract` hands it over: the check below compares a
        // resolved parent against it.
        let into = &directory.path().canonicalize().unwrap();
        let mut extraction = Extraction::default();
        let mut budget = 16;

        let bomb = vec![b'x'; 4096];
        place(
            into,
            &entry("bomb.bin"),
            &mut bomb.as_slice(),
            &mut budget,
            &mut extraction,
        );

        assert!(extraction.over_budget);
        assert_eq!(extraction.files, 0);
        assert_eq!(extraction.bytes, 0);
        assert_eq!(budget, 0);
        // A file cut off at the budget is not the file the archive holds, so
        // what was written is taken back rather than left as a plausible one.
        assert!(!into.join("bomb.bin").exists());
    }

    /// The seconds an archive records, as the date it is shown by.
    #[test]
    fn dates_are_read_the_way_both_formats_write_them() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        // A leap day, and the last second before one.
        assert_eq!(civil(1_709_164_800), (2024, 2, 29, 0, 0, 0));
        assert_eq!(civil(1_709_164_799), (2024, 2, 28, 23, 59, 59));
        // The Windows file time 7z records, at the unix epoch and past it.
        assert_eq!(unix_seconds(11_644_473_600 * 10_000_000), None);
        assert_eq!(unix_seconds(11_644_473_601 * 10_000_000), Some(1));
        // A file that recorded no time at all is not a date in 1601.
        assert_eq!(unix_seconds(0), None);
    }
}
