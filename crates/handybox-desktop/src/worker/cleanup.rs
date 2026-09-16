//! Disk cleanup jobs: choosing a folder, reading it, and moving a checked
//! selection to the trash.
//!
//! A scan and a removal are separate commands on purpose. Nothing here can be
//! reached without the page having shown what was found first, and the removal
//! carries the plan that was confirmed rather than a selection to re-interpret.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::cleanup::{self, Mode, Plan, Removal, ScanOutcome};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc},
};

pub enum Command {
    Choose(Language),
    /// A folder the user already chose: a drop, or a second scan of the same
    /// place after something was removed from it. The flag travels with the
    /// job so that asking a different question can stop this one; the worker
    /// runs one command at a time, and a scan nobody is waiting for would
    /// otherwise hold the queue for the whole folder.
    Scan {
        root: PathBuf,
        mode: Mode,
        cancel: Arc<AtomicBool>,
    },
    Remove {
        root: PathBuf,
        plan: Plan,
    },
}

pub enum Event {
    Chosen(PathBuf),
    Scanned(std::result::Result<ScanOutcome, Failure>),
    Removed(Removal),
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Cleanup(event));
    };
    match command {
        Command::Choose(language) => {
            let folder = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a folder to scan", "选择要扫描的文件夹"))
                    .pick_folder(),
            );
            match folder {
                Some(folder) => send(Event::Chosen(folder.path().to_owned())),
                None => {
                    let _ = events.send(Outgoing::Finished(Ok(Message::Cancelled)));
                }
            }
        }
        Command::Scan { root, mode, cancel } => send(Event::Scanned(
            cleanup::scan_until(&root, mode, &cancel).map_err(Failure::cleanup),
        )),
        // Every target is checked again inside `remove`, immediately before it
        // is trashed; a file that changed since the scan is skipped there.
        Command::Remove { root, plan } => send(Event::Removed(cleanup::remove(&root, &plan))),
    }
}
