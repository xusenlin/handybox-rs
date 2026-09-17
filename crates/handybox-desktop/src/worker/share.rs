//! LAN share jobs: opening the folder to the network, and everything done to
//! what is in it.
//!
//! Unlike every other tool here, one of these commands leaves something running.
//! `Start` hands back a [`Server`] that the worker keeps in [`Shared`], exactly
//! as the clipboard workspace keeps its watcher: the thing that outlives the
//! command lives beside the clipboard context, not in the controller, because
//! the worker thread is the only place that survives without touching the event
//! loop. Nothing else about the shape changes — a command still goes in and an
//! event still comes out.
use super::{Event as Outgoing, Shared};
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::share::{self, Entry, Server};
use std::{path::PathBuf, sync::mpsc};

pub enum Command {
    /// Open the folder to the network. Carries the language so the served page
    /// opens in the one the desktop is showing.
    Start {
        root: PathBuf,
        chinese: bool,
    },
    Stop,
    /// Re-read the shared folder. Sent on a timer while the page is open, so it
    /// goes out through `dispatch` and never takes the busy state.
    List {
        visible: bool,
    },
    Choose(Language),
    /// Pick the folder to share. Only offered while stopped.
    ChooseRoot(Language),
    Import(Vec<PathBuf>),
    SaveText(String),
    /// Read a shared `.txt` back, to show it without leaving the app.
    ReadText(String),
    /// Hand a shared file to the desktop's own file manager.
    Reveal(String),
    /// Move a shared file to the system trash.
    Delete(String),
    /// Tell the running server which language to serve. Cheap enough that the
    /// controller sends it on every language change without checking.
    Language(bool),
}

pub enum Event {
    /// Sharing began. Everything the page shows about a running server is here,
    /// so the controller never has to ask the worker a question.
    Started {
        url: String,
        urls: Vec<String>,
        root: PathBuf,
        reachable: bool,
        /// The address as pixels, or `None` when it is too long to encode.
        qr: Option<share::Qr>,
    },
    Stopped,
    /// The server stopped without being asked, which only an OS-level failure
    /// causes. Reported once, on the tick that noticed.
    Lost,
    Listed(std::result::Result<Vec<Entry>, Failure>),
    /// A folder was chosen to share from. Saving the preference is the
    /// controller's job; this only reports the choice.
    RootChosen(PathBuf),
    /// The contents of a shared text file, to show in place.
    Text {
        name: String,
        text: String,
    },
    TextSaved(String),
}

pub fn execute(command: Command, shared: &mut Shared, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Share(event));
    };
    let finish = |outcome| {
        let _ = events.send(Outgoing::Finished(outcome));
    };
    match command {
        Command::Start { root, chinese } => {
            match share::start(root, share::DEFAULT_PORT, chinese) {
                Ok(server) => {
                    send(Event::Started {
                        url: server.url().to_owned(),
                        urls: server.urls().to_vec(),
                        root: server.root().to_owned(),
                        reachable: server.reachable(),
                        // A failure here is not a failure to share: the address is
                        // on screen either way, and the code is a convenience.
                        qr: share::qr(server.url(), QR_INK).ok(),
                    });
                    // Listing immediately means the page is never briefly empty for
                    // a folder that already has files in it.
                    list(&server, &send);
                    shared.server = Some(server);
                    finish(Ok(Message::Sharing));
                }
                Err(error) => finish(Err(Failure::share(error))),
            }
        }
        Command::Stop => {
            // Dropping the server is what stops it; `stop` only makes the wait
            // for transfers in flight explicit.
            if let Some(server) = shared.server.take() {
                server.stop();
            }
            send(Event::Listed(Ok(Vec::new())));
            send(Event::Stopped);
            finish(Ok(Message::Stopped));
        }
        Command::List { visible } => {
            let Some(server) = &shared.server else { return };
            if server.failed() {
                shared.server = None;
                send(Event::Listed(Ok(Vec::new())));
                send(Event::Lost);
                return;
            }
            if visible {
                list(server, &send);
            }
        }
        Command::Language(chinese) => {
            if let Some(server) = &shared.server {
                server.set_chinese(chinese);
            }
        }
        Command::ChooseRoot(language) => {
            let folder = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a folder to share", "选择要共享的文件夹"))
                    .pick_folder(),
            );
            match folder {
                Some(folder) => {
                    send(Event::RootChosen(folder.path().to_owned()));
                    finish(Ok(Message::Ready));
                }
                None => finish(Ok(Message::Cancelled)),
            }
        }
        Command::Choose(language) => {
            let files = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose files to share", "选择要共享的文件"))
                    .pick_files(),
            );
            match files {
                Some(files) => import(
                    shared,
                    files
                        .into_iter()
                        .map(|file| file.path().to_owned())
                        .collect(),
                    &send,
                    &finish,
                ),
                None => finish(Ok(Message::Cancelled)),
            }
        }
        Command::Import(paths) => import(shared, paths, &send, &finish),
        Command::SaveText(text) => {
            let Some(server) = &shared.server else {
                return finish(Err(Failure::not_sharing()));
            };
            let store = server.store();
            match store.save_text(&text) {
                Ok(name) => {
                    send(Event::TextSaved(text));
                    list(server, &send);
                    finish(Ok(Message::Shared {
                        files: 1,
                        failed: 0,
                        first: Some(name),
                    }));
                }
                Err(error) => finish(Err(Failure::share(error))),
            }
        }
        Command::ReadText(name) => {
            let Some(server) = &shared.server else {
                return finish(Err(Failure::not_sharing()));
            };
            let outcome = server
                .store()
                .read_text(&name)
                .map_err(Failure::share)
                .map(|text| Event::Text { name, text });
            match outcome {
                Ok(event) => {
                    send(event);
                    finish(Ok(Message::Ready));
                }
                Err(failure) => finish(Err(failure)),
            }
        }
        Command::Reveal(name) => {
            let Some(server) = &shared.server else {
                return finish(Err(Failure::not_sharing()));
            };
            let outcome = server
                .store()
                .locate(&name)
                .map_err(Failure::share)
                .and_then(|path| crate::link::reveal(&path).map_err(Failure::operation));
            finish(outcome.map(|()| Message::Ready));
        }
        Command::Delete(name) => {
            let Some(server) = &shared.server else {
                return finish(Err(Failure::not_sharing()));
            };
            let outcome = server
                .store()
                .delete(&name)
                .map_err(Failure::share)
                .map(|()| Message::Trashed {
                    files: 1,
                    freed: 0,
                    skipped: 0,
                });
            list(server, &send);
            finish(outcome);
        }
    }
}

/// The ink the QR code is drawn in: the palette's accent, so the code reads as
/// part of the card rather than as a pasted-in picture.
const QR_INK: [u8; 3] = [0x3c, 0x52, 0x73];

fn list(server: &Server, send: &impl Fn(Event)) {
    send(Event::Listed(server.store().list().map_err(Failure::share)));
}

/// Copy files into the shared folder, reporting what actually went in. One that
/// fails does not stop the rest: a batch dropped onto the window can easily hold
/// something this tool will not take.
fn import(
    shared: &Shared,
    paths: Vec<PathBuf>,
    send: &impl Fn(Event),
    finish: &impl Fn(std::result::Result<Message, Failure>),
) {
    let Some(server) = &shared.server else {
        return finish(Err(Failure::not_sharing()));
    };
    let store = server.store();
    let mut shared_names = Vec::new();
    let mut failed = 0;
    for path in paths {
        match store.import(&path) {
            Ok(name) => shared_names.push(name),
            Err(_) => failed += 1,
        }
    }
    list(server, send);
    finish(Ok(Message::Shared {
        files: shared_names.len(),
        failed,
        first: shared_names.into_iter().next(),
    }));
}
