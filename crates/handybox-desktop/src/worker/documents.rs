//! Document converter jobs: native pickers, conversion and export.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use anyhow::Result;
use handybox_core::tools::documents::{self, ConvertedDocument};
use std::{path::PathBuf, sync::mpsc};

pub enum Command {
    Pick(Language),
    Convert(PathBuf),
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
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let result = match command {
        Command::Pick(language) => {
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
        Command::Export {
            language,
            source,
            name,
            markdown,
        } => export(language, source, name, markdown).map_err(Failure::document),
    };
    let _ = events.send(Outgoing::Finished(result));
}

fn convert(path: PathBuf, events: &mpsc::Sender<Outgoing>) {
    let _ = events.send(Outgoing::Documents(Event::Started(path.clone())));
    let result = documents::convert(&path).map_err(Failure::document);
    let _ = events.send(Outgoing::Documents(Event::Converted(result)));
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
