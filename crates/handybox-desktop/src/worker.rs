//! One bounded worker keeps parsing and file I/O off the Slint event loop.
use crate::locale::{Failure, FailureKind, Language, Message};
use anyhow::{Context, Result};
use clipboard_rs::{Clipboard, ClipboardContext};
use handybox_core::tools::documents::{self, ConvertedDocument};
use std::{path::PathBuf, sync::mpsc, thread};

pub enum Command {
    PickDocument(Language),
    Convert(PathBuf),
    Copy(String),
    Export {
        language: Language,
        source: PathBuf,
        name: String,
        markdown: String,
    },
}

pub enum Event {
    Started(PathBuf),
    Converted(std::result::Result<ConvertedDocument, Failure>),
    Finished(std::result::Result<Message, Failure>),
}

pub struct Worker {
    pub commands: mpsc::SyncSender<Command>,
    pub events: mpsc::Receiver<Event>,
}

impl Worker {
    pub fn start() -> Result<Self> {
        let (commands, input) = mpsc::sync_channel(1);
        let (output, events) = mpsc::channel();
        thread::Builder::new()
            .name("handybox-worker".into())
            .spawn(move || {
                // Reuse one OS context; on X11 it also owns a selection-serving thread.
                let mut clipboard = None;
                while let Ok(command) = input.recv() {
                    // A parser panic must not leave the desktop permanently busy.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        execute(command, &output, &mut clipboard)
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

fn execute(
    command: Command,
    events: &mpsc::Sender<Event>,
    clipboard: &mut Option<ClipboardContext>,
) {
    let result = match command {
        Command::PickDocument(language) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Choose a document to convert", "选择要转换的文档"))
                    .add_filter(language.text("Documents", "文档"), documents::EXTENSIONS)
                    .add_filter(language.text("All files", "所有文件"), &["*"])
                    .pick_file(),
            );
            if let Some(file) = file {
                convert(file.path().to_owned(), events);
                return;
            }
            Ok(Message::Cancelled)
        }
        Command::Convert(path) => {
            convert(path, events);
            return;
        }
        Command::Copy(text) => copy(text, clipboard)
            .map(|()| Message::Copied)
            .map_err(|error| Failure {
                kind: FailureKind::Clipboard,
                detail: format!("{error:#}"),
            }),
        Command::Export {
            language,
            source,
            name,
            markdown,
        } => export(language, source, name, markdown).map_err(Failure::document),
    };
    let _ = events.send(Event::Finished(result));
}

fn convert(path: PathBuf, events: &mpsc::Sender<Event>) {
    let _ = events.send(Event::Started(path.clone()));
    let result = documents::convert(&path).map_err(Failure::document);
    let _ = events.send(Event::Converted(result));
}

fn copy(text: String, context: &mut Option<ClipboardContext>) -> Result<()> {
    if context.is_none() {
        *context = Some(
            ClipboardContext::new().map_err(|e| anyhow::anyhow!("Clipboard unavailable: {e}"))?,
        );
    }
    let clipboard = context.as_ref().context("Clipboard unavailable")?;
    clipboard
        .set_text(text)
        .map_err(|e| anyhow::anyhow!("Could not copy text: {e}"))
}

fn export(language: Language, source: PathBuf, name: String, markdown: String) -> Result<Message> {
    let selected = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Export Markdown", "导出 Markdown"))
            .set_file_name(&name)
            .add_filter("Markdown", &["md"])
            .save_file(),
    );
    let Some(file) = selected else {
        return Ok(Message::Cancelled);
    };
    // Do not silently change an extension after the native overwrite prompt.
    documents::export_markdown(&source, file.path(), &markdown)?;
    Ok(Message::Saved(file.path().to_owned()))
}
