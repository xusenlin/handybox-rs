//! A folder on this machine, offered to the other devices on the same network.
//!
//! Every other tool in this box answers a question and finishes. This one is a
//! *service*: it holds a port open until it is told to stop, and while it is
//! open, machines that are not this one can put files into a folder that is.
//! That is a different thing to be careful about, so the shape here is narrow on
//! purpose.
//!
//! What the server will do is fixed at compile time and does not grow: list a
//! single flat folder, take an upload into it, hand one back, and save a scrap
//! of text as a `.txt`. There is no path parameter that is ever joined onto the
//! root without going through [`validate_name`] first, no way to name a
//! subdirectory, and nothing is served from anywhere but the one folder the
//! person running it chose.
//!
//! The runtime lives in [`Server`] and is created by [`start`]. Nothing spawns a
//! thread, binds a socket or enters async code until someone asks to share, so a
//! session that never opens this tool never pays for it.
//!
//! Errors are [`ShareIssue`] like everywhere else, and the browser gets
//! [`ShareIssue::code`] rather than a sentence: the page carries both languages
//! and picks its own, exactly as the desktop does.
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path as UrlPath, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

/// Stable error categories for callers to localize without matching error
/// strings. The browser gets [`ShareIssue::code`] and looks the wording up in
/// its own table, so an API response never carries display text either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareIssue {
    /// The shared folder could not be created where it was asked for.
    FolderCreate,
    /// The shared folder is a symlink. Everything served is resolved under the
    /// root, so the root itself must be a real directory.
    FolderSymlink,
    FolderReadOnly,
    FolderRead,
    /// The chosen port and the ones after it are all taken.
    PortsBusy,
    ServerStart,
    /// The server stopped on its own, which only an OS-level failure causes.
    ServerStopped,
    /// A name that cannot be a file in the shared folder: a path, a hidden file,
    /// a control character.
    InvalidName,
    /// A name Windows will not give a file, refused on every platform so a
    /// folder stays portable.
    ReservedName,
    /// The request did not come from the page this server serves. Only a
    /// browser ever sees this one.
    Refused,
    NotFile,
    TooLarge,
    TextEmpty,
    TextTooLarge,
    /// Ten thousand files already share this name.
    NameCollision,
    FileMissing,
    Read,
    Write,
    Delete,
    /// The address is too long to fit a QR code at the size the page draws.
    QrTooLong,
    /// The upload did not arrive intact: a lost connection, a truncated form.
    UploadFailed,
}

impl ShareIssue {
    /// What the browser is told. A stable token, never a sentence: the page
    /// holds both languages and looks this up in its own table.
    pub fn code(self) -> &'static str {
        match self {
            Self::FolderCreate => "folder-create",
            Self::FolderSymlink => "folder-symlink",
            Self::FolderReadOnly => "folder-read-only",
            Self::FolderRead => "folder-read",
            Self::PortsBusy => "ports-busy",
            Self::ServerStart => "server-start",
            Self::ServerStopped => "server-stopped",
            Self::InvalidName => "invalid-name",
            Self::ReservedName => "reserved-name",
            Self::Refused => "refused",
            Self::NotFile => "not-file",
            Self::TooLarge => "too-large",
            Self::TextEmpty => "text-empty",
            Self::TextTooLarge => "text-too-large",
            Self::NameCollision => "name-collision",
            Self::FileMissing => "file-missing",
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
            Self::QrTooLong => "qr-too-long",
            Self::UploadFailed => "upload-failed",
        }
    }

    /// The status a browser should see. Everything the request itself got wrong
    /// is a 400; everything this machine got wrong is a 500.
    fn status(self) -> StatusCode {
        match self {
            Self::InvalidName | Self::ReservedName | Self::NotFile | Self::TextEmpty => {
                StatusCode::BAD_REQUEST
            }
            Self::TooLarge | Self::TextTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Refused => StatusCode::FORBIDDEN,
            Self::FileMissing => StatusCode::NOT_FOUND,
            Self::UploadFailed => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for ShareIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FolderCreate => "Could not create the shared folder. Please choose another one",
            Self::FolderSymlink => "The shared folder must not be a symbolic link",
            Self::FolderReadOnly => "The shared folder is not writable. Please choose another one",
            Self::FolderRead => "Could not read the shared folder",
            Self::PortsBusy => "This port and the twenty after it are all in use",
            Self::ServerStart => "Could not start the sharing server",
            Self::ServerStopped => "The sharing server stopped unexpectedly",
            Self::InvalidName => {
                "Invalid file name: paths, hidden files and special characters are not shared"
            }
            Self::ReservedName => "This name is reserved on Windows",
            Self::Refused => "Open the sharing address shown in HandyBox, then try again",
            Self::NotFile => "Please choose a file; compress folders first",
            Self::TooLarge => "A single file cannot exceed 10 GiB",
            Self::TextEmpty => "Please enter some text first",
            Self::TextTooLarge => "Text cannot exceed 1 MiB",
            Self::NameCollision => "Too many files share this name; rename it and try again",
            Self::FileMissing => "This file is no longer in the shared folder",
            Self::Read => "Could not read this file",
            Self::Write => "Could not write to the shared folder",
            Self::Delete => "Could not move this file to the trash",
            Self::QrTooLong => "This address is too long to show as a QR code",
            Self::UploadFailed => "The upload did not arrive intact. Please try again",
        })
    }
}
impl std::error::Error for ShareIssue {}

/// Big enough that the limit is never the reason a transfer failed on a LAN, and
/// small enough to be a real ceiling on what one request can write.
pub const MAX_UPLOAD_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// A scrap of text — a link, a note, a token — not a document. Documents are
/// files, and files have their own route.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// The default port, and the first of twenty-one tried in order.
pub const DEFAULT_PORT: u16 = 8765;
/// How many ports after the default are tried before giving up.
const PORT_ATTEMPTS: u16 = 20;
/// A text file is named after how it starts, so the list can be read without
/// opening anything.
const TEXT_NAME_CHARS: usize = 28;
/// Downloads read this much at a time. `ReaderStream` defaults to 4 KiB, which
/// on a gigabyte file is a quarter of a million reads and a quarter of a million
/// chunks through the whole HTTP pipeline.
const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
/// Uploads buffer this much before touching the disk. Every `write` on tokio's
/// `File` costs a trip to a blocking thread, and the multipart chunks arriving
/// are far smaller than this.
const UPLOAD_BUFFER_BYTES: usize = 256 * 1024;
/// The folder offered by default, under the user's home directory. Named rather
/// than derived so that someone who shared with a previous version finds the
/// same folder here.
pub const DEFAULT_FOLDER_NAME: &str = "LanDropData";

/// The default shared folder: `~/LanDropData`.
pub fn default_root() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(DEFAULT_FOLDER_NAME))
}

/// One file in the shared folder, as both the page and the desktop see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// Seconds since the Unix epoch. Formatted where it is displayed, never
    /// here: the two front ends format dates differently.
    pub modified: u64,
    /// Whether this is a `.txt` that can be read in place rather than downloaded.
    pub is_text: bool,
}

/// The shared folder. Every path this tool touches is built here, from the root
/// plus one validated name — there is no second way in.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Open a folder for sharing, creating it if it is not there. Fails now
    /// rather than on the first upload if it cannot be written to.
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).context(ShareIssue::FolderCreate)?;
        if fs::symlink_metadata(&root)
            .context(ShareIssue::FolderRead)?
            .file_type()
            .is_symlink()
        {
            bail!(ShareIssue::FolderSymlink);
        }
        let root = root.canonicalize().context(ShareIssue::FolderRead)?;
        tempfile::Builder::new()
            .prefix(".handybox-share-")
            .tempfile_in(&root)
            .context(ShareIssue::FolderReadOnly)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // A disconnected or replaced folder must not be recreated by a refresh.
    fn check_root(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.root).context(ShareIssue::FolderRead)?;
        if metadata.file_type().is_symlink() {
            bail!(ShareIssue::FolderSymlink);
        }
        if !metadata.is_dir() {
            bail!(ShareIssue::FolderRead);
        }
        Ok(())
    }

    /// Everything shared, newest first. Anything that is not a plain file with a
    /// name this tool would have accepted is skipped rather than reported: the
    /// folder belongs to the user, who may well have put other things in it.
    pub fn list(&self) -> Result<Vec<Entry>> {
        self.check_root()?;
        let mut entries = Vec::new();
        for item in fs::read_dir(&self.root).context(ShareIssue::FolderRead)? {
            let Ok(item) = item else { continue };
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if validate_name(&name).is_err() || !item.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let Ok(meta) = item.metadata() else { continue };
            entries.push(Entry {
                // Not `to_ascii_lowercase`: that allocates a String per file, and
                // this runs over the whole folder every couple of seconds.
                is_text: meta.len() <= MAX_TEXT_BYTES as u64
                    && name
                        .rsplit_once('.')
                        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("txt")),
                name,
                size: meta.len(),
                modified: meta
                    .modified()
                    .unwrap_or(UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            });
        }
        entries.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
        Ok(entries)
    }

    /// A scratch file in the shared folder itself, so that publishing it is a
    /// rename rather than a copy across filesystems.
    fn temporary(&self) -> Result<tempfile::NamedTempFile> {
        self.check_root()?;
        tempfile::Builder::new()
            .prefix(".handybox-share-")
            .tempfile_in(&self.root)
            .context(ShareIssue::Write)
    }

    /// Publish a finished scratch file under `name`, or the first free
    /// `name (n)` beside it. Never overwrites: two people uploading the same
    /// name from different phones both keep their file.
    fn commit(&self, mut temp: tempfile::NamedTempFile, name: &str) -> Result<String> {
        validate_name(name)?;
        self.check_root()?;
        let path = Path::new(name);
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let extension = path
            .extension()
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_default();
        for index in 0..10_000 {
            let candidate = if index == 0 {
                name.to_owned()
            } else {
                format!("{stem} ({index}){extension}")
            };
            match temp.persist_noclobber(self.root.join(&candidate)) {
                Ok(_) => return Ok(candidate),
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    temp = error.file
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error.error).context(ShareIssue::Write));
                }
            }
        }
        bail!(ShareIssue::NameCollision)
    }

    /// Copy a file from anywhere on this machine into the shared folder.
    pub fn import(&self, source: &Path) -> Result<String> {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context(ShareIssue::InvalidName)?;
        validate_name(name)?;
        let mut input = fs::File::open(source).context(ShareIssue::Read)?;
        let meta = input.metadata().context(ShareIssue::Read)?;
        if !meta.is_file() {
            bail!(ShareIssue::NotFile);
        }
        if meta.len() > MAX_UPLOAD_BYTES {
            bail!(ShareIssue::TooLarge);
        }
        let mut temp = self.temporary()?;
        // Both sides are concrete `File`s so `io::copy` can take the platform's
        // fast path; wrapping either in `Take` or in the `NamedTempFile` falls
        // back to copying block by block. The size was checked above, and is
        // checked again here against a source growing mid-copy.
        let size = std::io::copy(&mut input, temp.as_file_mut()).context(ShareIssue::Write)?;
        if size > MAX_UPLOAD_BYTES {
            bail!(ShareIssue::TooLarge);
        }
        temp.as_file().sync_all().context(ShareIssue::Write)?;
        self.commit(temp, name)
    }

    /// Save a scrap of text as a `.txt`, named after how it starts.
    pub fn save_text(&self, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            bail!(ShareIssue::TextEmpty);
        }
        if text.len() > MAX_TEXT_BYTES {
            bail!(ShareIssue::TextTooLarge);
        }
        let name = text_file_name(text);
        let mut temp = self.temporary()?;
        temp.write_all(text.as_bytes()).context(ShareIssue::Write)?;
        temp.as_file().sync_all().context(ShareIssue::Write)?;
        self.commit(temp, &name)
    }

    /// Open a shared file for reading. Refuses anything that is not a plain
    /// file, and refuses to follow a link out of the shared folder — the caller
    /// here may be a stranger on the network.
    pub fn open(&self, name: &str) -> Result<fs::File> {
        validate_name(name)?;
        self.check_root()?;
        let path = self.root.join(name);
        if !fs::symlink_metadata(&path)
            .context(ShareIssue::FileMissing)?
            .is_file()
        {
            bail!(ShareIssue::NotFile);
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let file = options.open(&path).context(ShareIssue::FileMissing)?;
        let meta = file.metadata().context(ShareIssue::Read)?;
        if !meta.is_file() {
            bail!(ShareIssue::NotFile);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                bail!(ShareIssue::NotFile);
            }
        }
        Ok(file)
    }

    /// The full path of a shared file, for handing to the desktop's file
    /// manager. Validated the same way as everything else.
    pub fn locate(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        self.check_root()?;
        let path = self.root.join(name);
        if !fs::symlink_metadata(&path)
            .context(ShareIssue::FileMissing)?
            .is_file()
        {
            bail!(ShareIssue::NotFile);
        }
        Ok(path)
    }

    /// Stop sharing a file, by moving it to the platform's trash. Nothing in
    /// this box unlinks a file the user can still see: a wrong tap on a phone
    /// should be recoverable from the desktop it happened on.
    ///
    /// Not reachable from the network. Deleting is a decision for the person at
    /// the machine doing the sharing.
    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.locate(name)?;
        trash::delete(&path).context(ShareIssue::Delete)
    }

    /// Read a shared `.txt` back, for showing in place.
    pub fn read_text(&self, name: &str) -> Result<String> {
        let file = self.open(name)?;
        if file.metadata().context(ShareIssue::Read)?.len() > MAX_TEXT_BYTES as u64 {
            bail!(ShareIssue::TextTooLarge);
        }
        let mut text = String::new();
        // One byte past the limit is enough to know it was exceeded, and stops
        // a file that grew since the check from being read without a bound.
        file.take((MAX_TEXT_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .context(ShareIssue::Read)?;
        if text.len() > MAX_TEXT_BYTES {
            bail!(ShareIssue::TextTooLarge);
        }
        Ok(text)
    }
}

/// Name a text file after its first line, so the list can be read at a glance.
/// Anything a file name cannot hold becomes a space; text that starts with
/// nothing usable falls back to a timestamp.
fn text_file_name(text: &str) -> String {
    let stem: String = text
        .trim_start()
        .chars()
        .take(TEXT_NAME_CHARS)
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let name = format!(
        "{}.txt",
        stem.trim_matches(|c: char| c == '.' || c.is_whitespace())
    );
    if validate_name(&name).is_ok() {
        return name;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("text-{stamp}.txt")
}

/// The only gate between a name that arrived over the network and a path on this
/// disk. A name that passes is a single portable file name in one flat folder:
/// it cannot separate, cannot be hidden, and cannot be a Windows device.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        bail!(ShareIssue::InvalidName);
    }
    // Compared case-insensitively throughout rather than upper-casing first:
    // that would allocate a String per file, and `list` calls this for every
    // entry in the folder every couple of seconds.
    let base = name.split('.').next().unwrap_or("").as_bytes();
    let device = base.len() == 4
        && (base[..3].eq_ignore_ascii_case(b"COM") || base[..3].eq_ignore_ascii_case(b"LPT"))
        && matches!(base[3], b'1'..=b'9');
    if device
        || [&b"CON"[..], b"PRN", b"AUX", b"NUL"]
            .iter()
            .any(|reserved| base.eq_ignore_ascii_case(reserved))
    {
        bail!(ShareIssue::ReservedName);
    }
    Ok(())
}

/// A running server. Dropping it stops the server and shuts the runtime down;
/// there is no way to have one of the two without the other.
pub struct Server {
    /// Kept in an Option so that [`Server::stop`] can wait for the shutdown,
    /// while `drop` on an ordinary quit does not have to.
    runtime: Option<tokio::runtime::Runtime>,
    stop: tokio::sync::watch::Sender<bool>,
    /// Set when the server stopped on its own, which only an OS-level failure
    /// causes. Polled by the desktop rather than pushed, because the desktop is
    /// already looking at this tool on a timer.
    failed: Arc<AtomicBool>,
    /// Which language the served page shows. Shared rather than fixed at start,
    /// so switching the desktop's language reaches a phone on the next reload.
    chinese: Arc<AtomicBool>,
    store: Store,
    task: Option<tokio::task::JoinHandle<()>>,
    port: u16,
    urls: Vec<String>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every address this machine can be reached on, best first.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    /// The one address to show and encode. Always present: loopback is the
    /// fallback when this machine is on no network at all.
    pub fn url(&self) -> &str {
        &self.urls[0]
    }

    /// Whether the address is reachable from another device, rather than being
    /// the loopback fallback.
    pub fn reachable(&self) -> bool {
        !self.url().starts_with("http://127.")
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Whether the server has stopped on its own since it started.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Follow the desktop's language on the next page load.
    pub fn set_chinese(&self, chinese: bool) {
        self.chinese.store(chinese, Ordering::Relaxed);
    }

    /// Stop serving and wait for transfers in flight to finish, up to a few
    /// seconds. Called when the user stops sharing; quitting takes the same path
    /// through `drop`.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.stop.send(true);
        if let Some(runtime) = self.runtime.take() {
            // Shutting down the runtime immediately cancels async transfers.
            // Keep it running while Axum drains, with a bounded total wait.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            if let Some(task) = self.task.take() {
                runtime.block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
                });
            }
            runtime.shutdown_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Open `root` to the network and start serving it.
///
/// Binding happens on this thread, before the runtime exists, so that "the port
/// is taken" is an ordinary error the caller gets back rather than something
/// that has to be reported out of an async task.
pub fn start(root: PathBuf, port: u16, chinese: bool) -> Result<Server> {
    let store = Store::new(root)?;
    let listener = bind(port)?;
    let port = listener
        .local_addr()
        .context(ShareIssue::ServerStart)?
        .port();
    let urls = lan_urls(port);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("handybox-share")
        .build()
        .context(ShareIssue::ServerStart)?;
    let chinese = Arc::new(AtomicBool::new(chinese));
    let failed = Arc::new(AtomicBool::new(false));
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let api = Api {
        store: store.clone(),
        chinese: chinese.clone(),
    };
    let reporting = failed.clone();
    let task = {
        let _guard = runtime.enter();
        let listener =
            tokio::net::TcpListener::from_std(listener).context(ShareIssue::ServerStart)?;
        runtime.spawn(async move {
            let served = axum::serve(listener, router(api))
                .with_graceful_shutdown(async move {
                    let _ = stopped.changed().await;
                })
                .await;
            // Reaching here with an error means the listener itself died. The
            // desktop notices on its next tick and says the sharing stopped.
            if served.is_err() {
                reporting.store(true, Ordering::Relaxed);
            }
        })
    };
    Ok(Server {
        runtime: Some(runtime),
        stop,
        failed,
        chinese,
        store,
        task: Some(task),
        port,
        urls,
    })
}

/// Take the first free port at or after `port`, up to [`PORT_ATTEMPTS`] later.
/// Bound before the runtime starts, so a taken port is a plain error.
fn bind(port: u16) -> Result<std::net::TcpListener> {
    let last = port.saturating_add(if port == 0 { 0 } else { PORT_ATTEMPTS });
    for candidate in port..=last {
        match std::net::TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            candidate,
        )) {
            Ok(listener) => {
                listener
                    .set_nonblocking(true)
                    .context(ShareIssue::ServerStart)?;
                return Ok(listener);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => return Err(anyhow::Error::new(error).context(ShareIssue::ServerStart)),
        }
    }
    bail!(ShareIssue::PortsBusy)
}

/// Every address this machine answers on, in the order a person would want to
/// try them: the interface that actually carries traffic first, private
/// addresses before public ones.
fn lan_urls(port: u16) -> Vec<String> {
    let mut ips: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() => {
                Some(ip)
            }
            _ => None,
        })
        .collect();
    // Which interface the OS would actually route through. Connecting a UDP
    // socket sends nothing; it only makes the kernel pick a source address.
    let preferred = std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|probe| {
            probe.connect("192.0.2.1:80").ok()?;
            probe.local_addr().ok().map(|address| address.ip())
        });
    ips.sort_by_key(|ip| (!ip.is_private(), Some(IpAddr::V4(*ip)) != preferred, *ip));
    ips.dedup();
    let mut urls: Vec<String> = ips
        .into_iter()
        .map(|ip| format!("http://{ip}:{port}"))
        .collect();
    if urls.is_empty() {
        urls.push(format!("http://127.0.0.1:{port}"));
    }
    urls
}

/// A square of pixels, ready to become an image wherever images are made. The
/// desktop layer owns that conversion; this crate knows nothing about a renderer.
pub struct Qr {
    pub size: u32,
    pub rgba: Vec<u8>,
}

/// Draw the sharing address as a QR code, in the ink colour the caller asks for.
/// Use whole pixels per module, with exactly four quiet modules on each side.
/// The canvas follows the code instead of padding it out to a fixed size.
pub fn qr(url: &str, ink: [u8; 3]) -> Result<Qr> {
    const TARGET_SIZE: usize = 108;
    const QUIET_ZONE: usize = 4;
    let code = qrcode::QrCode::new(url.as_bytes()).context(ShareIssue::QrTooLong)?;
    let modules = code.width() + QUIET_ZONE * 2;
    if modules > TARGET_SIZE {
        bail!(ShareIssue::QrTooLong);
    }
    let scale = (TARGET_SIZE + modules / 2) / modules;
    let size = modules * scale;
    let margin = QUIET_ZONE * scale;
    let mut rgba = vec![0_u8; size * size * 4];
    let dot = [ink[0], ink[1], ink[2], 255];
    for y in 0..code.width() {
        for x in 0..code.width() {
            if code[(x, y)] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..scale {
                let start = ((margin + y * scale + dy) * size + margin + x * scale) * 4;
                for pixel in rgba[start..start + scale * 4].chunks_exact_mut(4) {
                    pixel.copy_from_slice(&dot);
                }
            }
        }
    }
    Ok(Qr {
        size: size as u32,
        rgba,
    })
}

// ---------------------------------------------------------------------------
// The HTTP surface. Four routes and a download, and nothing that takes a path.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Api {
    store: Store,
    chinese: Arc<AtomicBool>,
}

/// An error on its way to a browser. Carries the category, never a sentence:
/// the page holds both languages and looks the code up itself.
struct ApiError(ShareIssue);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0.status(),
            Json(serde_json::json!({ "error": self.0.code() })),
        )
            .into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self(
            error
                .downcast_ref::<ShareIssue>()
                .copied()
                .unwrap_or(ShareIssue::Write),
        )
    }
}

impl From<ShareIssue> for ApiError {
    fn from(issue: ShareIssue) -> Self {
        Self(issue)
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// Run a blocking file operation without holding an async worker. Every route
/// here touches the disk, and none of them do it on the runtime's own threads.
async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T> + Send + 'static,
) -> ApiResult<T> {
    match tokio::task::spawn_blocking(job).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(ApiError(ShareIssue::Write)),
    }
}

fn router(api: Api) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/api/files", get(list))
        .route(
            "/api/upload",
            // A megabyte over the file ceiling, for the multipart envelope
            // around it.
            post(upload).layer(DefaultBodyLimit::max(
                (MAX_UPLOAD_BYTES + 1024 * 1024) as usize,
            )),
        )
        .route(
            "/api/text",
            // Six bytes per character is the worst a JSON-escaped string can be.
            post(save_text).layer(DefaultBodyLimit::max(MAX_TEXT_BYTES * 6 + 1024)),
        )
        .route("/api/text/{name}", get(read_text))
        .route("/download/{name}", get(download))
        .layer(middleware::from_fn(guard))
        .with_state(api)
}

/// What keeps this from being an open door for any page the browser happens to
/// have open. Nothing here is a substitute for being on a trusted network; it is
/// the part that stops a *website* from reaching a server running on the LAN.
async fn guard(request: Request, next: Next) -> Response {
    // The desktop advertises IP literals. Refuse arbitrary DNS names so a
    // website cannot rebind its own origin to this listener and bypass CORS.
    if let Some(host) = request.headers().get(header::HOST) {
        let allowed = host
            .to_str()
            .ok()
            .and_then(|host| host.parse::<axum::http::uri::Authority>().ok())
            .is_some_and(|authority| {
                authority.host().eq_ignore_ascii_case("localhost")
                    || authority.host().parse::<Ipv4Addr>().is_ok()
            });
        if !allowed {
            return ApiError(ShareIssue::Refused).into_response();
        }
    }
    // No CORS headers are ever granted, so a cross-site request cannot read a
    // reply. Refusing it outright means it cannot cause a write either.
    if request.uri().path() != "/"
        && request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|site| site == "cross-site")
    {
        return ApiError(ShareIssue::Refused).into_response();
    }
    if request.method() == axum::http::Method::POST {
        let headers = request.headers();
        let origin_matches_host = match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            // `strip_prefix` rather than formatting the host into a new string:
            // equivalent, and one allocation fewer on every upload.
            Some(origin) => {
                origin.strip_prefix("http://")
                    == headers
                        .get(header::HOST)
                        .and_then(|host| std::str::from_utf8(host.as_bytes()).ok())
            }
            None => true,
        };
        // A header no HTML form can set: a cross-origin form POST cannot carry
        // it, so reaching a write route requires our own page's script.
        if !origin_matches_host || headers.get("x-handybox-share").is_none_or(|v| v != "1") {
            return ApiError(ShareIssue::Refused).into_response();
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
             form-action 'self'",
        ),
    );
    response
}

/// The one page. Both languages are in it; which one it opens in is decided
/// here, from whatever the desktop is showing right now.
async fn page(State(api): State<Api>) -> Html<String> {
    const PAGE: &str = include_str!("../../web/index.html");
    let language = if api.chinese.load(Ordering::Relaxed) {
        "zh-CN"
    } else {
        "en"
    };
    Html(PAGE.replace("__HANDYBOX_LANG__", language))
}

async fn list(State(api): State<Api>) -> ApiResult<Json<Vec<Entry>>> {
    Ok(Json(blocking(move || api.store.list()).await?))
}

async fn upload(
    State(api): State<Api>,
    mut multipart: Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    let mut names = Vec::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| ShareIssue::UploadFailed)?
    {
        let name = field.file_name().ok_or(ShareIssue::InvalidName)?.to_owned();
        validate_name(&name)?;
        let store = api.store.clone();
        let (temp, file) = blocking(move || {
            let temp = store.temporary()?;
            let file = temp.reopen().context(ShareIssue::Write)?;
            Ok((temp, file))
        })
        .await?;
        let mut output = tokio::io::BufWriter::with_capacity(
            UPLOAD_BUFFER_BYTES,
            tokio::fs::File::from_std(file),
        );
        let mut size = 0_u64;
        while let Some(chunk) = field.chunk().await.map_err(|_| ShareIssue::UploadFailed)? {
            size += chunk.len() as u64;
            if size > MAX_UPLOAD_BYTES {
                return Err(ShareIssue::TooLarge.into());
            }
            output
                .write_all(&chunk)
                .await
                .map_err(|_| ShareIssue::Write)?;
        }
        output.flush().await.map_err(|_| ShareIssue::Write)?;
        let file = output.into_inner();
        file.sync_all().await.map_err(|_| ShareIssue::Write)?;
        drop(file);
        let store = api.store.clone();
        names.push(blocking(move || store.commit(temp, &name)).await?);
    }
    if names.is_empty() {
        return Err(ShareIssue::UploadFailed.into());
    }
    Ok(Json(serde_json::json!({ "names": names })))
}

#[derive(Deserialize)]
struct TextInput {
    text: String,
}

async fn save_text(
    State(api): State<Api>,
    Json(input): Json<TextInput>,
) -> ApiResult<Json<serde_json::Value>> {
    let name = blocking(move || api.store.save_text(&input.text)).await?;
    Ok(Json(serde_json::json!({ "name": name })))
}

async fn read_text(
    State(api): State<Api>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let text = blocking(move || api.store.read_text(&name)).await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

async fn download(State(api): State<Api>, UrlPath(name): UrlPath<String>) -> ApiResult<Response> {
    let requested = name.clone();
    // The open and the stat go to a blocking thread together: no synchronous
    // file call happens on an async worker.
    let (file, size) = blocking(move || {
        let file = api.store.open(&requested)?;
        let size = file.metadata().context(ShareIssue::Read)?.len();
        Ok((file, size))
    })
    .await?;
    let encoded = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
    let mut response = Body::from_stream(ReaderStream::with_capacity(
        tokio::fs::File::from_std(file),
        DOWNLOAD_CHUNK_BYTES,
    ))
    .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    // The ASCII fallback is deliberately a placeholder: a browser that cannot
    // read the UTF-8 form would otherwise be handed a mangled name.
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"download\"; filename*=UTF-8''{encoded}"
        ))
        .map_err(|_| ShareIssue::InvalidName)?,
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn api(root: &Path) -> Api {
        Api {
            store: Store::new(root.to_owned()).unwrap(),
            chinese: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn names_are_portable_and_cannot_escape() {
        for name in [
            "",
            "..",
            "../secret",
            "a/b",
            "a\\b",
            "x:y",
            ".hidden",
            "trailing.",
            "trailing ",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name:?}");
        }
        for name in ["CON", "nul.txt", "COM1", "lpt9.log"] {
            assert_eq!(
                validate_name(name)
                    .unwrap_err()
                    .downcast_ref::<ShareIssue>()
                    .copied(),
                Some(ShareIssue::ReservedName),
                "expected {name:?} to be reserved"
            );
        }
        for name in ["notes.txt", "你好.pdf", "COM0", "COMET", "a.b.c"] {
            assert!(validate_name(name).is_ok(), "rejected {name:?}");
        }
    }

    #[test]
    fn same_name_twice_keeps_both_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned()).unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("note.txt");
        std::fs::write(&source, "first").unwrap();
        assert_eq!(store.import(&source).unwrap(), "note.txt");
        std::fs::write(&source, "second").unwrap();
        assert_eq!(store.import(&source).unwrap(), "note (1).txt");
        assert_eq!(store.read_text("note.txt").unwrap(), "first");
        assert_eq!(store.read_text("note (1).txt").unwrap(), "second");
    }

    #[test]
    fn text_is_named_after_how_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned()).unwrap();
        assert_eq!(store.save_text("hello there").unwrap(), "hello there.txt");
        // A path separator in the first line must not become one in the name.
        assert_eq!(
            store.save_text("a/b c").unwrap(),
            "a b c.txt",
            "separators have to be flattened, not kept"
        );
        // Leading punctuation is stepped over rather than kept, so text that
        // opens with a divider is still named after its first real word.
        assert_eq!(store.save_text("...\n\nbody").unwrap(), "body.txt");
        // Nothing usable anywhere in the first line falls back to a timestamp
        // rather than producing a hidden or empty name.
        let stamped = store.save_text("...").unwrap();
        assert!(stamped.starts_with("text-"), "got {stamped}");
        assert!(store.save_text("   ").is_err());
    }

    #[test]
    fn the_scratch_files_are_never_listed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned()).unwrap();
        store.save_text("visible").unwrap();
        // A crashed upload leaves one of these behind; it is not shared.
        std::fs::write(dir.path().join(".handybox-share-abc"), "partial").unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "visible.txt");
        assert!(listed[0].is_text);
    }

    #[tokio::test]
    async fn text_round_trips_and_other_origins_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let app = router(api(dir.path()));
        let post = |origin: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/text")
                .header("host", "localhost:8765")
                .header("origin", origin)
                .header("x-handybox-share", "1")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"hello 世界"}"#))
                .unwrap()
        };
        assert_eq!(
            app.clone()
                .oneshot(post("http://evil.example"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = app
            .clone()
            .oneshot(post("http://localhost:8765"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let name = body["name"].as_str().unwrap();
        let encoded =
            percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/download/{encoded}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains("attachment")
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "hello 世界"
        );
    }

    #[tokio::test]
    async fn uploads_are_saved_and_traversal_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let api = api(dir.path());
        let store = api.store.clone();
        let app = router(api);
        for (name, status) in [
            ("你好.txt", StatusCode::OK),
            ("../escape.txt", StatusCode::BAD_REQUEST),
        ] {
            let body = format!(
                "--test\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\n\
                 Content-Type: text/plain\r\n\r\nhello\r\n--test--\r\n"
            );
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/upload")
                        .header("x-handybox-share", "1")
                        .header("content-type", "multipart/form-data; boundary=test")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "for {name}");
        }
        assert_eq!(store.read_text("你好.txt").unwrap(), "hello");
        assert_eq!(store.list().unwrap().len(), 1);
        // A form that stops mid-file must leave nothing behind but the file that
        // already arrived.
        let truncated = "--test\r\nContent-Disposition: form-data; name=\"files\"; \
                         filename=\"partial.txt\"\r\n\r\npartial";
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/upload")
                    .header("x-handybox-share", "1")
                    .header("content-type", "multipart/form-data; boundary=test")
                    .body(Body::from(truncated))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response.status().is_success());
        assert_eq!(std::fs::read_dir(store.root()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn a_write_without_our_own_header_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let api = api(dir.path());
        let store = api.store.clone();
        let response = router(api)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/text")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"from a form"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response.status().is_success());
        assert!(store.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn arbitrary_dns_hosts_cannot_rebind_to_the_share_server() {
        let dir = tempfile::tempdir().unwrap();
        let app = router(api(dir.path()));
        for (host, expected) in [
            ("evil.example:8765", StatusCode::FORBIDDEN),
            ("localhost:8765", StatusCode::OK),
            ("192.168.1.20:8765", StatusCode::OK),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[test]
    fn listing_does_not_recreate_a_disconnected_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared");
        let store = Store::new(root.clone()).unwrap();
        fs::remove_dir(&root).unwrap();
        assert!(store.list().is_err());
        assert!(store.save_text("hello").is_err());
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_roots_and_symlink_files_are_not_shared() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared");
        let store = Store::new(root.clone()).unwrap();
        fs::write(outside.path().join("private.txt"), "private").unwrap();
        symlink(outside.path().join("private.txt"), root.join("link.txt")).unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(store.open("link.txt").is_err());
        fs::remove_file(root.join("link.txt")).unwrap();
        fs::remove_dir(&root).unwrap();
        symlink(outside.path(), &root).unwrap();
        assert!(store.list().is_err());
        assert!(store.read_text("private.txt").is_err());
        assert!(store.save_text("hello").is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_text_remains_downloadable_without_offering_preview() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned()).unwrap();
        let file = fs::File::create(dir.path().join("large.txt")).unwrap();
        file.set_len((MAX_TEXT_BYTES + 1) as u64).unwrap();
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_text);
        assert!(store.open("large.txt").is_ok());
        assert!(store.read_text("large.txt").is_err());
    }

    #[test]
    fn concurrent_writes_keep_every_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_owned()).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let store = &store;
                scope.spawn(move || store.save_text("same name").unwrap());
            }
        });
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 8);
        for entry in entries {
            assert_eq!(store.read_text(&entry.name).unwrap(), "same name");
        }
    }

    #[test]
    fn stopping_releases_the_listening_port() {
        let dir = tempfile::tempdir().unwrap();
        let server = start(dir.path().to_owned(), 0, false).unwrap();
        let port = server.port();
        server.stop();
        assert!(std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok());
    }

    #[test]
    fn the_address_fits_a_qr_code() {
        let drawn = qr("http://192.168.1.23:8765", [60, 82, 115]).unwrap();
        assert_eq!(drawn.rgba.len(), (drawn.size * drawn.size * 4) as usize);
        assert!(
            drawn.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255),
            "the code has to have drawn something"
        );
        assert!(qr(&"x".repeat(8000), [0, 0, 0]).is_err());
    }

    #[test]
    fn compact_qr_codes_keep_the_quiet_zone_and_decode() {
        for url in ["http://10.0.0.1:8765", "http://192.168.123.123:65535"] {
            let drawn = qr(url, [60, 82, 115]).unwrap();
            let modules = qrcode::QrCode::new(url.as_bytes()).unwrap().width();
            let size = drawn.size as usize;
            let scale = size / (modules + 8);
            let margin = 4 * scale;
            let opaque: Vec<_> = drawn
                .rgba
                .chunks_exact(4)
                .enumerate()
                .filter_map(|(i, p)| (p[3] != 0).then_some((i % size, i / size)))
                .collect();
            assert_eq!(opaque.iter().map(|(x, _)| *x).min(), Some(margin));
            assert_eq!(opaque.iter().map(|(_, y)| *y).min(), Some(margin));
            assert!(
                opaque
                    .iter()
                    .all(|(x, y)| *x < size - margin && *y < size - margin)
            );
            let luma = drawn
                .rgba
                .chunks_exact(4)
                .map(|p| {
                    if p[3] == 0 {
                        255
                    } else {
                        ((u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])) / 3) as u8
                    }
                })
                .collect();
            let decoded = rxing::helpers::detect_in_luma(
                luma,
                drawn.size,
                drawn.size,
                Some(rxing::BarcodeFormat::QR_CODE),
            )
            .unwrap();
            assert_eq!(decoded.getText(), url);
        }
    }

    #[test]
    fn there_is_always_an_address_to_show() {
        let urls = lan_urls(8765);
        assert!(!urls.is_empty());
        assert!(urls.iter().all(|url| url.ends_with(":8765")));
    }
}
