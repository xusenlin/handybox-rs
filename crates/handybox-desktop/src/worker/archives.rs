//! Archive jobs: the two pickers, reading a table of contents, reading a
//! folder, and the two long runs that write files.
//!
//! Extraction and packing are one command each, reporting as they go. Sending
//! one command per entry would take the busy state a thousand times over and
//! interleave with anything else the window asked for; a single job that says
//! where it has got to keeps the page honest without giving up the
//! one-operation-at-a-time rule.
//!
//! The plan for a new archive travels as an `Arc`: it is a name and a path per
//! file, and a folder of twenty thousand of them is not worth copying into a
//! message.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::archives::{self, Kind, Listing, Plan};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub enum Command {
    /// Pick an archive to look inside.
    Choose(Language),
    /// Pick a folder to pack.
    ChooseFolder(Language),
    /// An archive the user already chose: a drop, or a command-line argument.
    Read(PathBuf),
    /// A folder the user already chose.
    Survey(PathBuf),
    /// Unpack the ticked entries into a folder the job asks for itself.
    Extract {
        language: Language,
        source: PathBuf,
        wanted: HashSet<String>,
        cancel: Arc<AtomicBool>,
    },
    /// Pack the ticked files into a file the job asks for itself.
    Create {
        language: Language,
        plan: Arc<Plan>,
        wanted: HashSet<String>,
        kind: Kind,
        cancel: Arc<AtomicBool>,
    },
}

pub enum Event {
    Chosen(PathBuf),
    Picked(PathBuf),
    Listed(std::result::Result<Listing, Failure>),
    Surveyed(std::result::Result<Arc<Plan>, Failure>),
    /// Where a run has got to, sent between entries.
    Progress {
        done: usize,
        total: usize,
    },
    /// A run is over, however it went. What it did travels on the global
    /// notice: neither the archive that was read nor the folder that was packed
    /// has changed, so the page has nothing to re-read and only a bar to put
    /// away.
    Done,
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Archives(event));
    };
    match command {
        Command::Choose(language) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose an archive", "选择压缩包"))
                    .add_filter(
                        language.text("ZIP and 7z archives", "ZIP 与 7z 压缩包"),
                        archives::EXTENSIONS,
                    )
                    .pick_file(),
            );
            match file {
                Some(file) => send(Event::Chosen(file.path().to_owned())),
                None => finish(events, Ok(Message::Cancelled)),
            }
        }
        Command::ChooseFolder(language) => {
            let folder = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a folder to pack", "选择要压缩的文件夹"))
                    .pick_folder(),
            );
            match folder {
                Some(folder) => send(Event::Picked(folder.path().to_owned())),
                None => finish(events, Ok(Message::Cancelled)),
            }
        }
        Command::Read(path) => send(Event::Listed(
            archives::list(&path).map_err(Failure::archive),
        )),
        Command::Survey(folder) => send(Event::Surveyed(
            archives::survey(&folder)
                .map(Arc::new)
                .map_err(Failure::archive),
        )),
        Command::Extract {
            language,
            source,
            wanted,
            cancel,
        } => extract(language, source, wanted, cancel, events),
        Command::Create {
            language,
            plan,
            wanted,
            kind,
            cancel,
        } => create(language, plan, wanted, kind, cancel, events),
    }
}

fn finish(events: &mpsc::Sender<Outgoing>, result: std::result::Result<Message, Failure>) {
    let _ = events.send(Outgoing::Finished(result));
}

fn extract(
    language: Language,
    source: PathBuf,
    wanted: HashSet<String>,
    cancel: Arc<AtomicBool>,
    events: &mpsc::Sender<Outgoing>,
) {
    let send = |event| {
        let _ = events.send(Outgoing::Archives(event));
    };
    let into = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Extract into which folder?", "解压到哪个文件夹？"))
            .pick_folder(),
    );
    let Some(into) = into else {
        send(Event::Done);
        return finish(events, Ok(Message::Cancelled));
    };
    // The page has been showing a dialog, not a progress bar. This is the first
    // moment there is anything to be in the middle of.
    let total = wanted.len();
    send(Event::Progress { done: 0, total });
    let progress = |done: usize| {
        let _ = events.send(Outgoing::Archives(Event::Progress { done, total }));
    };
    match archives::extract(&source, into.path(), &wanted, &cancel, &progress) {
        Ok(extraction) => {
            send(Event::Done);
            finish(
                events,
                Ok(Message::Unpacked {
                    files: extraction.files,
                    folders: extraction.folders,
                    bytes: extraction.bytes,
                    skipped: extraction.skipped,
                    skipped_name: extraction.first_skipped,
                    refused: extraction.refused,
                    failed: extraction.failed,
                    // Stopping at the budget is the same thing to read as
                    // stopping on request: what was written is complete, and
                    // what follows it was not written.
                    cancelled: extraction.cancelled || extraction.over_budget,
                    into: extraction.into,
                }),
            );
        }
        Err(error) => {
            send(Event::Done);
            finish(events, Err(Failure::archive(error)));
        }
    }
    cancel.store(false, Ordering::Relaxed);
}

fn create(
    language: Language,
    plan: Arc<Plan>,
    wanted: HashSet<String>,
    kind: Kind,
    cancel: Arc<AtomicBool>,
    events: &mpsc::Sender<Outgoing>,
) {
    let send = |event| {
        let _ = events.send(Outgoing::Archives(event));
    };
    let file = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Save the archive", "保存压缩包"))
            .set_file_name(archives::suggested_name(&plan.root, kind))
            .add_filter(
                match kind {
                    Kind::Zip => language.text("ZIP archive", "ZIP 压缩包"),
                    Kind::SevenZ => language.text("7z archive", "7z 压缩包"),
                },
                &[kind.extension()],
            )
            .save_file(),
    );
    let Some(file) = file else {
        send(Event::Done);
        return finish(events, Ok(Message::Cancelled));
    };
    let total = wanted.len();
    send(Event::Progress { done: 0, total });
    let progress = |done: usize| {
        let _ = events.send(Outgoing::Archives(Event::Progress { done, total }));
    };
    // The extension is not corrected silently: the native dialog has already
    // asked about the path as it was typed, and the core refuses a name it
    // cannot write honestly.
    match archives::create(&plan, &wanted, file.path(), kind, &cancel, &progress) {
        Ok(creation) => {
            send(Event::Done);
            finish(
                events,
                Ok(Message::Packed {
                    files: creation.files,
                    size: creation.size,
                    packed: creation.packed,
                    failed: creation.failed,
                    cancelled: creation.cancelled,
                    path: creation.path,
                }),
            );
        }
        Err(error) => {
            send(Event::Done);
            finish(events, Err(Failure::archive(error)));
        }
    }
    cancel.store(false, Ordering::Relaxed);
}
