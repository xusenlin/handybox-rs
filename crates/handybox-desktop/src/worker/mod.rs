//! One bounded worker keeps parsing and file I/O off the Slint event loop.
//!
//! Commands and events are grouped per tool: the shell only has to route them,
//! and a new tool adds a module here instead of another arm in one large enum.
pub mod archives;
pub mod cleanup;
pub mod clipboard;
pub mod codes;
pub mod crypto;
pub mod diff;
pub mod documents;
pub mod images;
pub mod json;

use crate::locale::{Failure, FailureKind, Message};
use anyhow::{Context, Result};
use clipboard_rs::{Clipboard, ClipboardContext, WatcherShutdown};
use std::{sync::mpsc, thread};

pub enum Command {
    Documents(documents::Command),
    Json(json::Command),
    Crypto(crypto::Command),
    Diff(diff::Command),
    Codes(codes::Command),
    Clipboard(clipboard::Command),
    Cleanup(cleanup::Command),
    Images(images::Command),
    Archives(archives::Command),
    /// Copying is the same operation whichever tool produced the text, and the
    /// clipboard context is shared, so it lives here rather than in both tools.
    Copy(String),
}

pub enum Event {
    Documents(documents::Event),
    Json(json::Event),
    Crypto(crypto::Event),
    Diff(diff::Event),
    Codes(codes::Event),
    Clipboard(clipboard::Event),
    Cleanup(cleanup::Event),
    Images(images::Event),
    Archives(archives::Event),
    /// A command that only produced a notice: a copy, an export, a cancelled
    /// dialog or a failure. Every tool's busy state ends here.
    Finished(std::result::Result<Message, Failure>),
}

pub struct Worker {
    pub commands: mpsc::SyncSender<Command>,
    pub events: mpsc::Receiver<Event>,
}

/// State a command may need that is expensive to build or must be reused.
pub struct Shared {
    clipboard: Option<ClipboardContext>,
    /// Alive only while the clipboard workspace is watching. Dropping it is
    /// what stops the watching thread, so nothing runs when no one asked.
    watcher: Option<WatcherShutdown>,
    /// The worker's own command channel, for the watching thread to report a
    /// change through. A change becomes an ordinary job, in turn with the rest.
    commands: mpsc::SyncSender<Command>,
}

impl Shared {
    /// Reuse one OS context; on X11 it also owns a selection-serving thread.
    fn clipboard(&mut self) -> std::result::Result<&ClipboardContext, Failure> {
        if self.clipboard.is_none() {
            match ClipboardContext::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => return Err(Failure::access(error.to_string())),
            }
        }
        self.clipboard
            .as_ref()
            .ok_or_else(|| Failure::access(String::new()))
    }

    pub fn copy(&mut self, text: String) -> std::result::Result<Message, Failure> {
        let clipboard = self.clipboard()?;
        match clipboard.set_text(text) {
            Ok(()) => Ok(Message::Copied),
            Err(error) => Err(Failure::access(error.to_string())),
        }
    }
}

impl Failure {
    /// The OS clipboard itself could not be reached. Distinct from the workspace
    /// tool's own issues, which are about what was on it.
    fn access(detail: String) -> Self {
        Self {
            kind: FailureKind::ClipboardAccess,
            detail,
        }
    }
}

impl Worker {
    pub fn start() -> Result<Self> {
        let (commands, input) = mpsc::sync_channel(1);
        let (output, events) = mpsc::channel();
        let reporting = commands.clone();
        thread::Builder::new()
            .name("handybox-worker".into())
            // Document and JSON parsers recurse into nested structures; a deeper
            // stack than the 2 MiB default buys headroom a check cannot.
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let mut shared = Shared {
                    clipboard: None,
                    watcher: None,
                    commands: reporting,
                };
                while let Ok(command) = input.recv() {
                    // A parser panic must not leave the desktop permanently busy.
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match command {
                            Command::Documents(command) => documents::execute(command, &output),
                            Command::Json(command) => json::execute(command, &output),
                            Command::Crypto(command) => crypto::execute(command, &output),
                            Command::Diff(command) => diff::execute(command, &output),
                            Command::Codes(command) => codes::execute(command, &output),
                            Command::Clipboard(command) => {
                                clipboard::execute(command, &mut shared, &output)
                            }
                            Command::Cleanup(command) => cleanup::execute(command, &output),
                            Command::Images(command) => images::execute(command, &output),
                            Command::Archives(command) => archives::execute(command, &output),
                            Command::Copy(text) => {
                                let _ = output.send(Event::Finished(shared.copy(text)));
                            }
                        }));
                    if result.is_err() {
                        let _ = output.send(Event::Finished(Err(Failure {
                            kind: FailureKind::Operation,
                            detail: String::new(),
                        })));
                    }
                }
            })
            .context("Could not start the background worker")?;
        Ok(Self { commands, events })
    }
}
