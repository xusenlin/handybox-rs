//! Find what is taking up room in a folder, and move only what was chosen to
//! the system trash.
//!
//! This is the one tool here that touches files someone did not hand it, so
//! every rule it has is about not deleting the wrong thing. A scan only reads.
//! Removing happens in a second step, from an explicit selection, and each path
//! is checked again at that moment: still inside the folder that was scanned,
//! still a real file rather than a link, still the size the scan recorded. What
//! changed underneath is skipped rather than deleted on an old assumption.
//!
//! Nothing is unlinked. Everything goes to the platform's own trash, so a wrong
//! click is recoverable in the place people already know to look.
//!
//! Duplicate detection compares **content**, never names or timestamps: files of
//! the same size are separated by a cheap hash of their first bytes, and only
//! what still collides is read in full. That order matters on a folder of media,
//! where thousands of files share a size and almost none share a beginning.
//!
//! The engine is this module plus `sha2` and `trash`. czkawka was the plan and
//! is not used: its current versions need a newer Rust than this workspace
//! pins, and the version that does build brings an audio, video and RAW-image
//! stack — around fifty crates — for three kinds of scan that are a few hundred
//! lines here.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Stable error categories for callers to localize without matching error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupIssue {
    InputMissing,
    Read,
    /// A file was chosen where a folder is needed: this tool scans a tree.
    NotFolder,
    NothingSelected,
    /// Every copy in a duplicate group was selected. A tool for removing extra
    /// copies must not be the thing that removes the last one.
    WholeGroup,
    /// A path that is not inside the folder that was scanned. Only reachable by
    /// acting on a stale result, which is exactly when it matters.
    Outside,
}

impl std::fmt::Display for CleanupIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputMissing => f.write_str("That folder could not be found"),
            Self::Read => f.write_str("Could not read the folder"),
            Self::NotFolder => f.write_str("Please choose a folder, not a file"),
            Self::NothingSelected => f.write_str("Nothing is selected"),
            Self::WholeGroup => f.write_str(
                "Every copy in a group is selected. Leave one copy of each file in place",
            ),
            Self::Outside => {
                f.write_str("That file is no longer inside the folder that was scanned")
            }
        }
    }
}
impl std::error::Error for CleanupIssue {}

/// Entries a single scan will look at. A cap rather than a promise: a scan that
/// reaches it reports what it found and says it stopped early.
pub const MAX_FILES: usize = 200_000;
/// Bytes a single scan will read to compare content.
pub const MAX_HASH_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// How long a scan may run before it hands back what it has.
pub const MAX_SECONDS: u64 = 60;
/// What a file is separated by before it is read in full. Files of the same
/// size are common; files that also begin the same way are nearly always
/// copies, so this turns most of a duplicate scan into one short read each.
pub const PARTIAL_BYTES: u64 = 64 * 1024;
/// How many of the biggest files are listed.
pub const LARGE_COUNT: usize = 100;
/// Deeper than any tree worth cleaning, and shallow enough to recurse safely.
pub const MAX_DEPTH: usize = 64;
/// Folders a scan never enters. Version control keeps deliberate copies that
/// mean something, and the rest belong to the system rather than to anyone.
pub const SKIPPED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".Trash",
    ".Spotlight-V100",
    ".fseventsd",
    "$RECYCLE.BIN",
    "System Volume Information",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Files with identical content, grouped.
    #[default]
    Duplicates,
    /// Files with nothing in them, and folders with nothing under them.
    Empty,
    /// The biggest files, largest first.
    Large,
}

/// Why a scan stopped before it ran out of folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Files,
    Bytes,
    Time,
    /// The caller asked a different question. The result is not a partial
    /// answer to keep — it is the answer to something nobody is asking.
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    /// Which set of identical files this belongs to. Only duplicates have one.
    pub group: Option<u32>,
    /// A folder with nothing under it, rather than a file.
    pub directory: bool,
}

#[derive(Clone, Debug)]
pub struct ScanOutcome {
    pub root: PathBuf,
    pub mode: Mode,
    /// In display order: duplicate groups by how much they waste, empty entries
    /// by path, biggest files first.
    pub entries: Vec<Entry>,
    pub groups: u32,
    /// Files the scan looked at, whether or not they ended up in the result.
    pub scanned: usize,
    /// What removing everything worth removing would free: every copy but one
    /// in each duplicate group, or the size of what was found.
    pub reclaimable: u64,
    pub elapsed: Duration,
    /// Set when the scan stopped early, so the page can say the result is a
    /// prefix rather than an answer.
    pub stopped: Option<Limit>,
}

/// One file the caller asked to remove, with what the scan believed about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub size: u64,
    pub directory: bool,
}

/// A checked selection: what would be removed, and what it would free. Built
/// before anything is shown for confirmation, so the number on the button and
/// the files that would go are the same list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub targets: Vec<Target>,
    pub freed: u64,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// What actually happened. Skipped entries are not failures: a file that moved
/// or changed since the scan is a file this tool should leave alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removal {
    /// What is now in the trash, so a caller holding the scan can drop exactly
    /// these from it instead of reading the whole folder again.
    pub gone: Vec<PathBuf>,
    pub freed: u64,
    /// No longer what the scan saw: gone, changed, or replaced by a link.
    pub skipped: usize,
    /// The trash refused them.
    pub failed: usize,
}

/// Read a folder and report what it holds. Reads only: nothing here writes,
/// moves or deletes anything.
pub fn scan(root: &Path, mode: Mode) -> Result<ScanOutcome> {
    scan_until(root, mode, &AtomicBool::new(false))
}

/// The same scan, stoppable. The flag is read between files and between the
/// blocks of a file being hashed, so asking a different question costs the wait
/// for one block rather than the wait for the whole folder.
pub fn scan_until(root: &Path, mode: Mode, cancel: &AtomicBool) -> Result<ScanOutcome> {
    let started = Instant::now();
    let root = root.canonicalize().context(CleanupIssue::InputMissing)?;
    let meta = fs::metadata(&root).context(CleanupIssue::Read)?;
    ensure!(meta.is_dir(), CleanupIssue::NotFolder);

    let mut walk = Walk::new(mode, started, cancel);
    walk.visit(&root, 0);
    let (entries, groups, reclaimable) = match mode {
        Mode::Duplicates => duplicates(&mut walk),
        Mode::Empty => empty(&mut walk),
        Mode::Large => large(&mut walk),
    };
    Ok(ScanOutcome {
        root,
        mode,
        entries,
        groups,
        scanned: walk.scanned,
        reclaimable,
        elapsed: started.elapsed(),
        stopped: walk.stopped,
    })
}

/// Check a selection against the result it came from. The rules live here
/// rather than in the page, because they are the difference between removing
/// extra copies and removing the only one.
pub fn plan(outcome: &ScanOutcome, selected: &[usize]) -> Result<Plan> {
    ensure!(!selected.is_empty(), CleanupIssue::NothingSelected);
    let mut targets = Vec::with_capacity(selected.len());
    let mut freed = 0;
    let mut taken: HashMap<u32, usize> = HashMap::new();
    for index in selected {
        let Some(entry) = outcome.entries.get(*index) else {
            continue;
        };
        if let Some(group) = entry.group {
            *taken.entry(group).or_default() += 1;
        }
        freed += entry.size;
        targets.push(Target {
            path: entry.path.clone(),
            size: entry.size,
            directory: entry.directory,
        });
    }
    ensure!(!targets.is_empty(), CleanupIssue::NothingSelected);
    // A duplicate group must keep a copy. Anything else would make this tool
    // the one that deleted the last one.
    for (group, count) in taken {
        let members = outcome
            .entries
            .iter()
            .filter(|entry| entry.group == Some(group))
            .count();
        ensure!(count < members, CleanupIssue::WholeGroup);
    }
    Ok(Plan { targets, freed })
}

/// Whether a target is still exactly what the scan saw. Checked immediately
/// before the trash, so a file that changed in between is left alone.
pub fn verify(root: &Path, target: &Target) -> Result<()> {
    ensure!(target.path.starts_with(root), CleanupIssue::Outside);
    // Not `metadata`: a symlink that appeared where a file was must not be
    // followed, and must not be taken for the file it points at.
    let meta = fs::symlink_metadata(&target.path).context(CleanupIssue::InputMissing)?;
    ensure!(!meta.file_type().is_symlink(), CleanupIssue::Read);
    if target.directory {
        ensure!(meta.is_dir(), CleanupIssue::Read);
        // Something was put in it since the scan; an empty folder it is not.
        ensure!(
            fs::read_dir(&target.path)
                .context(CleanupIssue::Read)?
                .next()
                .is_none(),
            CleanupIssue::Read
        );
    } else {
        ensure!(meta.is_file(), CleanupIssue::Read);
        ensure!(meta.len() == target.size, CleanupIssue::Read);
    }
    Ok(())
}

/// Move a checked plan to the system trash.
///
/// Every target is verified first and one at a time — that is the check that
/// keeps a file which changed under us out of the trash — but the move itself
/// is a single trip. Asking the platform once per file makes it one operation
/// per file: on macOS that is an `osascript` and a Finder trash sound each, so
/// clearing forty duplicates played forty sounds and took as many round trips.
/// One call is also one undo step, which is how a person would think of it.
pub fn remove(root: &Path, plan: &Plan) -> Removal {
    let mut removal = Removal::default();
    let mut ready: Vec<&Target> = Vec::new();
    for target in &plan.targets {
        match verify(root, target) {
            Ok(()) => ready.push(target),
            Err(_) => removal.skipped += 1,
        }
    }
    if ready.is_empty() {
        return removal;
    }
    let paths: Vec<&Path> = ready.iter().map(|target| target.path.as_path()).collect();
    if trash::delete_all(&paths).is_ok() {
        for target in ready {
            removal.freed += target.size;
            removal.gone.push(target.path.clone());
        }
        return removal;
    }
    // A refusal says nothing about which path it choked on, and the platform
    // may have taken some of them before it stopped. Asking again could trash
    // something twice or report a file that is already gone as a failure, so
    // the folder itself is the answer: what is no longer there, went.
    for target in ready {
        if fs::symlink_metadata(&target.path).is_err() {
            removal.freed += target.size;
            removal.gone.push(target.path.clone());
        } else {
            removal.failed += 1;
        }
    }
    removal
}

/// Everything a walk collected, and why it stopped if it did.
struct Walk<'a> {
    mode: Mode,
    started: Instant,
    cancel: &'a AtomicBool,
    /// Every file worth comparing or listing: size first, because that is what
    /// both remaining modes sort and group by.
    files: Vec<(u64, PathBuf)>,
    empty_files: Vec<PathBuf>,
    empty_dirs: Vec<PathBuf>,
    scanned: usize,
    read: u64,
    stopped: Option<Limit>,
}

impl<'a> Walk<'a> {
    fn new(mode: Mode, started: Instant, cancel: &'a AtomicBool) -> Self {
        Self {
            mode,
            started,
            cancel,
            files: Vec::new(),
            empty_files: Vec::new(),
            empty_dirs: Vec::new(),
            scanned: 0,
            read: 0,
            stopped: None,
        }
    }

    /// Whether the scan may keep going. A scan that stops says so; it never
    /// pretends the prefix it has is the whole folder.
    fn running(&mut self) -> bool {
        if self.stopped.is_some() {
            return false;
        }
        if self.cancel.load(Ordering::Relaxed) {
            self.stopped = Some(Limit::Cancelled);
        } else if self.scanned >= MAX_FILES {
            self.stopped = Some(Limit::Files);
        } else if self.read >= MAX_HASH_BYTES {
            self.stopped = Some(Limit::Bytes);
        } else if self.started.elapsed() >= Duration::from_secs(MAX_SECONDS) {
            self.stopped = Some(Limit::Time);
        }
        self.stopped.is_none()
    }

    /// Walk one folder. Returns whether it is empty — no files anywhere under
    /// it, and every folder under it empty as well — which is what makes a tree
    /// of empty folders one finding rather than none.
    fn visit(&mut self, dir: &Path, depth: usize) -> bool {
        if depth >= MAX_DEPTH || !self.running() {
            return false;
        }
        let Ok(listing) = fs::read_dir(dir) else {
            // An unreadable folder is not an empty one, and not a failure
            // either: a scan of a large tree will meet a few.
            return false;
        };
        let mut empty = true;
        for entry in listing.flatten() {
            if !self.running() {
                return false;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                empty = false;
                continue;
            };
            // Links are never followed, compared or removed: what they point at
            // belongs to someone else, and a link is not a copy of anything.
            if kind.is_symlink() {
                empty = false;
                continue;
            }
            if kind.is_dir() {
                if SKIPPED
                    .iter()
                    .any(|name| entry.file_name().to_string_lossy() == *name)
                {
                    empty = false;
                    continue;
                }
                if !self.visit(&path, depth + 1) {
                    empty = false;
                }
                continue;
            }
            empty = false;
            self.scanned += 1;
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() == 0 {
                if self.mode == Mode::Empty {
                    self.empty_files.push(path);
                }
                continue;
            }
            if self.mode != Mode::Empty {
                self.files.push((meta.len(), path));
            }
        }
        if empty && self.mode == Mode::Empty {
            self.empty_dirs.push(dir.to_owned());
        }
        empty
    }

    /// Read a file's beginning, or all of it, counting what was read against
    /// the scan's budget.
    fn digest(&mut self, path: &Path, bytes: Option<u64>) -> Option<[u8; 32]> {
        let mut file = File::open(path).ok()?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut left = bytes.unwrap_or(u64::MAX);
        while left > 0 {
            // A single large file is a long read of its own; a scan nobody is
            // waiting for should not finish it first.
            if self.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let want = buffer.len().min(left as usize);
            let read = file.read(&mut buffer[..want]).ok()?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            left -= read as u64;
            self.read += read as u64;
        }
        Some(hasher.finalize().into())
    }
}

/// Files with identical content, grouped, most wasteful group first.
fn duplicates(walk: &mut Walk) -> (Vec<Entry>, u32, u64) {
    // Same size is the only free signal there is; everything else costs a read.
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (size, path) in std::mem::take(&mut walk.files) {
        by_size.entry(size).or_default().push(path);
    }
    let mut candidates: Vec<(u64, Vec<PathBuf>)> = by_size
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    // Largest first: a scan that runs out of budget should have spent it where
    // the space actually is.
    candidates.sort_by_key(|(size, _)| std::cmp::Reverse(*size));

    let mut sets: Vec<(u64, Vec<PathBuf>)> = Vec::new();
    for (size, paths) in candidates {
        if !walk.running() {
            break;
        }
        // A short file is its own beginning; reading it twice would be waste.
        let split = if size > PARTIAL_BYTES {
            regroup(walk, &paths, Some(PARTIAL_BYTES))
        } else {
            vec![paths]
        };
        for group in split {
            if group.len() < 2 || !walk.running() {
                continue;
            }
            for mut confirmed in regroup(walk, &group, None) {
                if confirmed.len() < 2 {
                    continue;
                }
                confirmed.sort();
                sets.push((size, confirmed));
            }
        }
    }
    // What a group costs is the size of the copies that are not needed.
    sets.sort_by(|a, b| {
        let wasted = |(size, paths): &(u64, Vec<PathBuf>)| size * (paths.len() as u64 - 1);
        wasted(b)
            .cmp(&wasted(a))
            .then_with(|| a.1.first().cmp(&b.1.first()))
    });

    let mut entries = Vec::new();
    let mut reclaimable = 0;
    for (id, (size, paths)) in sets.iter().enumerate() {
        reclaimable += size * (paths.len() as u64 - 1);
        for path in paths {
            entries.push(Entry {
                path: path.clone(),
                size: *size,
                group: Some(id as u32),
                directory: false,
            });
        }
    }
    (entries, sets.len() as u32, reclaimable)
}

/// Split a set of same-sized files by what they actually contain.
fn regroup(walk: &mut Walk, paths: &[PathBuf], bytes: Option<u64>) -> Vec<Vec<PathBuf>> {
    let mut by_digest: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
    for path in paths {
        if !walk.running() {
            break;
        }
        // A file that cannot be read is not a copy of anything: leave it out
        // rather than group it with the files it failed to match.
        if let Some(digest) = walk.digest(path, bytes) {
            by_digest.entry(digest).or_default().push(path.clone());
        }
    }
    by_digest.into_values().collect()
}

/// Files with nothing in them, then folders with nothing under them.
fn empty(walk: &mut Walk) -> (Vec<Entry>, u32, u64) {
    let mut files = std::mem::take(&mut walk.empty_files);
    let mut dirs = std::mem::take(&mut walk.empty_dirs);
    files.sort();
    dirs.sort();
    // A folder that only contains other empty folders is reported once, as
    // itself: removing it takes the tree under it, and listing every level
    // would ask for the same folder to be removed several times.
    dirs.dedup_by(|inner, outer| inner.starts_with(&*outer));
    let mut entries: Vec<Entry> = files
        .into_iter()
        .map(|path| Entry {
            path,
            size: 0,
            group: None,
            directory: false,
        })
        .collect();
    entries.extend(dirs.into_iter().map(|path| Entry {
        path,
        size: 0,
        group: None,
        directory: true,
    }));
    // Nothing is reclaimed by removing nothing: an empty file costs an inode,
    // not bytes, and saying "0 B" is the honest answer.
    (entries, 0, 0)
}

/// The biggest files, largest first.
fn large(walk: &mut Walk) -> (Vec<Entry>, u32, u64) {
    let mut files = std::mem::take(&mut walk.files);
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    files.truncate(LARGE_COUNT);
    let reclaimable = files.iter().map(|(size, _)| *size).sum();
    let entries = files
        .into_iter()
        .map(|(size, path)| Entry {
            path,
            size,
            group: None,
            directory: false,
        })
        .collect();
    (entries, 0, reclaimable)
}
