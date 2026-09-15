//! Barcode reader jobs: choosing an image, and recognizing what is in it.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::codes::{self, ScanOutcome};
use std::{path::PathBuf, sync::mpsc};

pub enum Command {
    Choose(Language),
    /// A path the user already chose: a drop, or a command-line argument.
    Scan(PathBuf),
}

pub enum Event {
    /// The picker returned a file. Recognition is a second command, so the page
    /// can show the image and say it is working before the pass begins.
    Chosen(PathBuf),
    Scanned(std::result::Result<ScanOutcome, Failure>),
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Codes(event));
    };
    match command {
        Command::Choose(language) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose an image", "选择图片"))
                    .add_filter(language.text("Images", "图片"), codes::EXTENSIONS)
                    .add_filter(language.text("All files", "所有文件"), &["*"])
                    .pick_file(),
            );
            match file {
                Some(file) => send(Event::Chosen(file.path().to_owned())),
                None => {
                    let _ = events.send(Outgoing::Finished(Ok(Message::Cancelled)));
                }
            }
        }
        Command::Scan(path) => send(Event::Scanned(codes::scan(&path).map_err(Failure::codes))),
    }
}
