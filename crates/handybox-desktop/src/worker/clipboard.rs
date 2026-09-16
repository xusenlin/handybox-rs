//! Clipboard workspace jobs: reading the clipboard, putting something back on
//! it, and watching it for changes.
//!
//! Every one of these needs the shared OS context, so they run here rather than
//! anywhere near the event loop — on X11 that context owns a selection-serving
//! thread, and reading a picture out of it is megabytes of decoding.
//!
//! Watching is a thread of its own, because the platforms disagree about what a
//! clipboard change is: an event on Windows and X11, a counter to poll on macOS.
//! The watcher does none of the reading — it only says *something changed*, and
//! the read that follows is an ordinary job on this worker.
use super::{Command as Job, Event as Outgoing, Shared};
use crate::locale::{Failure, Language, Message};
use anyhow::{Context, Result, anyhow, bail};
use clipboard_rs::{
    Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext,
    ContentFormat, RustImageData, WatcherShutdown, common::RustImage,
};
use handybox_core::tools::clipboard::{
    self as workspace, ClipboardIssue, Content, PREVIEW_EDGE, Picture, Thumbnail,
};
use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

pub enum Command {
    /// Read what is on the clipboard now, because the user asked for it.
    Capture,
    /// The watcher saw the clipboard change: the same read, without a busy state
    /// and without a complaint when there is nothing worth keeping.
    Notice,
    /// Put an item back on the clipboard.
    Restore(Content),
    /// Write a collected picture out as a file. Only a picture: a text or a
    /// path leaves through the clipboard, which is where it came from.
    Save {
        language: Language,
        png: Vec<u8>,
    },
    Watch(bool),
}

pub enum Event {
    Captured(std::result::Result<Content, Failure>),
    Noticed(Content),
    /// Whether the clipboard is being watched now. Sent for both answers: a
    /// watcher that could not start must not leave the switch claiming it did.
    Watching(bool),
}

pub fn execute(command: Command, shared: &mut Shared, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Clipboard(event));
    };
    match command {
        Command::Capture => send(Event::Captured(capture(shared))),
        // Collecting on its own says nothing when there is nothing to say: a
        // clipboard holding an unsupported format is not a failed operation,
        // because no one asked for this read.
        Command::Notice => {
            if let Ok(content) = capture(shared) {
                send(Event::Noticed(content));
            }
        }
        Command::Restore(content) => {
            let _ = events.send(Outgoing::Finished(restore(shared, content)));
        }
        Command::Save { language, png } => {
            let _ = events.send(Outgoing::Finished(
                save(language, &png).map_err(Failure::clipboard),
            ));
        }
        Command::Watch(wanted) => {
            let outcome = watch(shared, wanted);
            send(Event::Watching(outcome.is_ok() && wanted));
            let _ = events.send(Outgoing::Finished(outcome.map(|()| {
                if wanted {
                    Message::Watching
                } else {
                    Message::Unwatched
                }
            })));
        }
    }
}

/// Everything on the clipboard that this workspace keeps.
fn capture(shared: &mut Shared) -> std::result::Result<Content, Failure> {
    let clipboard = shared.clipboard()?;
    read(clipboard)
        .and_then(workspace::accept)
        .map_err(Failure::clipboard)
}

/// One clipboard can hold the same thing in several formats at once. The order
/// here is what a paste elsewhere would most likely mean: the files a file
/// manager put there, then a picture, and text last, because a copied file also
/// leaves its path behind as text.
fn read(clipboard: &ClipboardContext) -> Result<Content> {
    if clipboard.has(ContentFormat::Files) {
        let files = clipboard
            .get_files()
            .map_err(failed)
            .context(ClipboardIssue::Unsupported)?;
        let paths: Vec<PathBuf> = files
            .iter()
            .map(|reference| workspace::path_from_reference(reference))
            .collect();
        if !paths.is_empty() {
            return Ok(Content::Files(paths));
        }
    }
    if clipboard.has(ContentFormat::Image) {
        let image = clipboard
            .get_image()
            .map_err(failed)
            .context(ClipboardIssue::Unsupported)?;
        return Ok(Content::Image(picture(image)?));
    }
    if clipboard.has(ContentFormat::Text) {
        let text = clipboard
            .get_text()
            .map_err(failed)
            .context(ClipboardIssue::Unsupported)?;
        return Ok(Content::Text(text));
    }
    // Something is on the clipboard, but only in a format that would mean
    // nothing here — or there is nothing on it at all.
    if clipboard.has(ContentFormat::Rtf) || clipboard.has(ContentFormat::Html) {
        bail!(ClipboardIssue::Unsupported);
    }
    bail!(ClipboardIssue::Empty)
}

/// The picture as the workspace keeps it: the original encoded once, because
/// that is what goes back on the clipboard, and a thumbnail to draw.
fn picture(image: RustImageData) -> Result<Picture> {
    let (width, height) = image.get_size();
    let png = image
        .to_png()
        .map_err(failed)
        .context(ClipboardIssue::Unsupported)?
        .get_bytes()
        .to_vec();
    let thumbnail = image
        .thumbnail(PREVIEW_EDGE, PREVIEW_EDGE)
        .and_then(|small| small.to_rgba8())
        .map_err(failed)
        .context(ClipboardIssue::Unsupported)?;
    Ok(Picture {
        width,
        height,
        png,
        thumbnail: Thumbnail {
            width: thumbnail.width(),
            height: thumbnail.height(),
            rgba: thumbnail.into_raw(),
        },
    })
}

/// Put an item back where it came from. The watcher, if it is running, sees
/// this as a change like any other; the workspace recognizes the content it
/// already holds and moves that item to the top instead of keeping it twice.
fn restore(shared: &mut Shared, content: Content) -> std::result::Result<Message, Failure> {
    let clipboard = shared.clipboard()?;
    let written = match &content {
        Content::Text(text) => clipboard.set_text(text.clone()),
        Content::Files(paths) => clipboard.set_files(
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        ),
        Content::Image(picture) => {
            RustImageData::from_bytes(&picture.png).and_then(|image| clipboard.set_image(image))
        }
    };
    match written {
        Ok(()) => Ok(Message::Copied),
        Err(error) => Err(Failure::access(error.to_string())),
    }
}

/// Ask where the picture should go, and write it there.
fn save(language: Language, png: &[u8]) -> Result<Message> {
    let selected = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Save the picture", "保存图片"))
            .set_file_name("clipboard.png")
            .add_filter("PNG", &[workspace::PICTURE_EXTENSION])
            .save_file(),
    );
    let Some(file) = selected else {
        return Ok(Message::Cancelled);
    };
    // Do not silently change an extension after the native overwrite prompt.
    workspace::export(png, file.path())?;
    Ok(Message::Saved(file.path().to_owned()))
}

/// Start or stop the watcher. Stopping is dropping the handle: that closes the
/// channel the watching thread waits on, and the thread ends by itself.
fn watch(shared: &mut Shared, wanted: bool) -> std::result::Result<(), Failure> {
    if !wanted {
        shared.watcher = None;
        return Ok(());
    }
    if shared.watcher.is_some() {
        return Ok(());
    }
    shared.watcher = Some(start(shared.commands.clone())?);
    Ok(())
}

/// The handler runs on the watching thread and does as little as it can: the
/// reading happens on the worker, in turn with everything else.
struct Notifier {
    commands: mpsc::SyncSender<Job>,
}

impl ClipboardHandler for Notifier {
    fn on_clipboard_change(&mut self) {
        // The command channel holds one command, and it is also what a button
        // press needs: never block in it, or a click during a change would be
        // refused. A few attempts cover a worker that is briefly occupied.
        for attempt in 0..5 {
            if self
                .commands
                .try_send(Job::Clipboard(Command::Notice))
                .is_ok()
            {
                return;
            }
            if attempt < 4 {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn start(commands: mpsc::SyncSender<Job>) -> std::result::Result<WatcherShutdown, Failure> {
    let mut watcher =
        ClipboardWatcherContext::new().map_err(|error| Failure::access(error.to_string()))?;
    watcher.add_handler(Notifier { commands });
    let shutdown = watcher.get_shutdown_channel();
    thread::Builder::new()
        .name("handybox-clipboard".into())
        // Watching blocks until the shutdown channel closes, so it cannot share
        // the worker: nothing else would ever run there again.
        .spawn(move || watcher.start_watch())
        .map_err(|error| Failure::access(error.to_string()))?;
    Ok(shutdown)
}

/// The platform's own diagnostic, kept as the cause of a stable issue. Its type
/// is a boxed error the application never matches on, so only its words survive.
fn failed(error: Box<dyn std::error::Error + Send + Sync>) -> anyhow::Error {
    anyhow!("{error}")
}
