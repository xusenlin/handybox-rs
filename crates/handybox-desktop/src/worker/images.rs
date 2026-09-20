//! Image studio jobs: listing a folder, opening one picture, and writing a
//! batch into a folder someone chose.
//!
//! The decoded picture travels as an `Arc`. It is the one value here that is
//! expensive to move — a photograph is hundreds of megabytes once it is pixels
//! — and every render needs it again, so it is shared rather than sent back and
//! forth.
//!
//! A batch is one command that reports as it goes. Sending one command per
//! picture would take the busy state forty times and interleave with anything
//! else the window asked for; a single job that says where it has got to keeps
//! the page honest without giving up the one-operation-at-a-time rule.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use handybox_core::tools::images::{self, Listing, Picture, Recipe};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub enum Command {
    Choose(Language),
    /// A folder the user already chose: a drop, or a command-line argument.
    Scan(PathBuf),
    /// One picture of the folder, to look at.
    Load(PathBuf),
    /// Every ticked picture, into a folder the job asks for itself.
    Batch {
        language: Language,
        sources: Vec<PathBuf>,
        recipe: Recipe,
        cancel: Arc<AtomicBool>,
    },
}

/// What a batch did, including the aggregate size change for files that were
/// successfully written.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Batch {
    pub total: usize,
    pub written: usize,
    /// A name that was already taken, or the picture that was read.
    pub skipped: usize,
    pub failed: usize,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub cancelled: bool,
}

pub enum Event {
    Chosen(PathBuf),
    Listed(std::result::Result<Listing, Failure>),
    Loaded(std::result::Result<Arc<Picture>, Failure>),
    /// Where a batch has got to, sent between pictures.
    Progress {
        done: usize,
        total: usize,
    },
    /// The batch is over; the detailed result follows on the global notice.
    Batched,
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Images(event));
    };
    match command {
        Command::Choose(language) => {
            let folder = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a folder of pictures", "选择图片所在的文件夹"))
                    .pick_folder(),
            );
            match folder {
                Some(folder) => send(Event::Chosen(folder.path().to_owned())),
                None => {
                    let _ = events.send(Outgoing::Finished(Ok(Message::Cancelled)));
                }
            }
        }
        Command::Scan(folder) => send(Event::Listed(images::list(&folder).map_err(Failure::image))),
        Command::Load(path) => send(Event::Loaded(
            images::load(&path).map(Arc::new).map_err(Failure::image),
        )),
        Command::Batch {
            language,
            sources,
            recipe,
            cancel,
        } => {
            let into = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Export into which folder?", "导出到哪个文件夹？"))
                    .pick_folder(),
            );
            let Some(into) = into else {
                let _ = events.send(Outgoing::Finished(Ok(Message::Cancelled)));
                return;
            };
            // The page has been showing a dialog, not a progress bar. This is
            // the first moment there is anything to be in the middle of.
            send(Event::Progress {
                done: 0,
                total: sources.len(),
            });
            let batch = run(&sources, &recipe, into.path(), &cancel, &send);
            send(Event::Batched);
            let _ = events.send(Outgoing::Finished(Ok(Message::Exported {
                total: batch.total,
                files: batch.written,
                skipped: batch.skipped,
                failed: batch.failed,
                before_bytes: batch.before_bytes,
                after_bytes: batch.after_bytes,
                cancelled: batch.cancelled,
            })));
        }
    }
}

/// Read, render and write one picture at a time, saying where it has got to.
/// One at a time is also what keeps the memory flat: a batch never holds more
/// than the picture it is working on.
fn run(
    sources: &[PathBuf],
    recipe: &Recipe,
    into: &std::path::Path,
    cancel: &AtomicBool,
    send: &impl Fn(Event),
) -> Batch {
    let mut batch = Batch {
        total: sources.len(),
        ..Default::default()
    };
    for (index, source) in sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            batch.cancelled = true;
            break;
        }
        send(Event::Progress {
            done: index,
            total: sources.len(),
        });
        let written = images::load(source).and_then(|picture| {
            let before = picture.bytes;
            let rendered = images::render(&picture, recipe)?;
            let after = rendered.size();
            images::export_into(&rendered, into, source).map(|_| (before, after))
        });
        match written {
            Ok((before, after)) => {
                batch.written += 1;
                batch.before_bytes += before;
                batch.after_bytes += after;
            }
            // A name that is taken and a picture that cannot be read are both
            // "left alone", which is what a batch should do with them.
            Err(error) => match error.downcast_ref::<images::ImageIssue>() {
                Some(images::ImageIssue::Exists | images::ImageIssue::SourceOverwrite) => {
                    batch.skipped += 1
                }
                _ => batch.failed += 1,
            },
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_received_before_the_first_picture_is_respected() {
        let cancel = AtomicBool::new(true);
        let batch = run(
            &[PathBuf::from("not-opened.png")],
            &Recipe::default(),
            std::path::Path::new("."),
            &cancel,
            &|_| panic!("cancelled work must not start"),
        );
        assert!(batch.cancelled);
        assert_eq!(batch.total, 1);
        assert_eq!(batch.written + batch.failed + batch.skipped, 0);
    }

    #[test]
    fn a_successful_batch_reports_counts_and_size_change() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.ppm");
        std::fs::write(&source, b"P6\n2 1\n255\n\xff\0\0\0\xff\0").unwrap();
        let into = directory.path().join("out");
        std::fs::create_dir(&into).unwrap();

        let batch = run(
            std::slice::from_ref(&source),
            &Recipe::default(),
            &into,
            &AtomicBool::new(false),
            &|_| {},
        );

        assert_eq!(batch.total, 1);
        assert_eq!(batch.written, 1);
        assert_eq!(batch.skipped + batch.failed, 0);
        assert_eq!(batch.before_bytes, std::fs::metadata(source).unwrap().len());
        assert_eq!(
            batch.after_bytes,
            std::fs::metadata(into.join("source.png")).unwrap().len()
        );
    }
}
