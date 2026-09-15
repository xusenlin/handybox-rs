//! Text diff jobs: opening a side from disk, comparing, and exporting the diff.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use anyhow::Result;
use handybox_core::tools::diff::{self, DiffOutcome, LoadedText, Options};
use std::{path::PathBuf, sync::mpsc};

/// Which editor a file or a result belongs to. The pair is symmetric, so the
/// side travels with the job instead of being duplicated into two commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

pub enum Command {
    Open(Language, Side),
    /// A path the user already chose: a drop, or a command-line argument.
    Load(Side, PathBuf),
    /// Both sides at once, which is what two paths on the command line mean.
    /// Each side still reports its own outcome: one unreadable file does not
    /// cost the user the other one.
    LoadPair(PathBuf, PathBuf),
    Compare {
        left: Text,
        right: Text,
        options: Options,
    },
    Export {
        language: Language,
        /// Both sides, so neither can be replaced by the diff between them.
        sources: Vec<PathBuf>,
        name: String,
        text: String,
    },
}

/// One side as the worker receives it: the editor's contents and the name that
/// goes into the diff header.
pub struct Text {
    pub name: String,
    pub text: String,
}

pub enum Event {
    Opened(Side, std::result::Result<LoadedText, Failure>),
    Compared(std::result::Result<DiffOutcome, Failure>),
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Diff(event));
    };
    let result = match command {
        Command::Open(language, side) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(match side {
                        Side::Left => language.text("Open the original", "打开原始文本"),
                        Side::Right => {
                            language.text("Open the changed version", "打开修改后的文本")
                        }
                    })
                    .add_filter(language.text("Text", "文本"), diff::EXTENSIONS)
                    .add_filter(language.text("All files", "所有文件"), &["*"])
                    .pick_file(),
            );
            let Some(file) = file else {
                return finish(events, Ok(Message::Cancelled));
            };
            send(Event::Opened(
                side,
                diff::load(file.path()).map_err(Failure::diff),
            ));
            return;
        }
        Command::Load(side, path) => {
            send(Event::Opened(
                side,
                diff::load(&path).map_err(Failure::diff),
            ));
            return;
        }
        Command::LoadPair(left, right) => {
            send(Event::Opened(
                Side::Left,
                diff::load(&left).map_err(Failure::diff),
            ));
            send(Event::Opened(
                Side::Right,
                diff::load(&right).map_err(Failure::diff),
            ));
            return;
        }
        Command::Compare {
            left,
            right,
            options,
        } => {
            send(Event::Compared(
                diff::compare(
                    diff::Side {
                        name: &left.name,
                        text: &left.text,
                    },
                    diff::Side {
                        name: &right.name,
                        text: &right.text,
                    },
                    options,
                )
                .map_err(Failure::diff),
            ));
            return;
        }
        Command::Export {
            language,
            sources,
            name,
            text,
        } => export(language, sources, name, text).map_err(Failure::diff),
    };
    finish(events, result);
}

fn finish(events: &mpsc::Sender<Outgoing>, result: std::result::Result<Message, Failure>) {
    let _ = events.send(Outgoing::Finished(result));
}

fn export(
    language: Language,
    sources: Vec<PathBuf>,
    name: String,
    text: String,
) -> Result<Message> {
    let selected = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Export the diff", "导出差异"))
            .set_file_name(&name)
            .add_filter(
                language.text("Unified diff", "统一差异格式"),
                &["diff", "patch"],
            )
            .save_file(),
    );
    let Some(file) = selected else {
        return Ok(Message::Cancelled);
    };
    // Do not silently change an extension after the native overwrite prompt.
    diff::export(&sources, file.path(), &text)?;
    Ok(Message::Saved(file.path().to_owned()))
}
