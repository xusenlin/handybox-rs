//! JSON workbench jobs: validation, jq runs, file open and export.
use super::Event as Outgoing;
use crate::locale::{Failure, Language, Message};
use anyhow::Result;
use handybox_core::tools::json::{self, JsonOutcome, JsonSummary, Layout, LoadedJson, Mode};
use std::{path::PathBuf, sync::mpsc};

pub enum Command {
    Open(Language),
    /// A path the user already chose: a drop, or a command-line argument.
    Load(PathBuf),
    /// Background check of the editor's contents. It never blocks the UI, so it
    /// is submitted silently and may be superseded by a later edit.
    Validate {
        input: String,
        mode: Mode,
    },
    Run {
        input: String,
        filter: String,
        layout: Layout,
        mode: Mode,
    },
    Export {
        language: Language,
        source: Option<PathBuf>,
        name: String,
        text: String,
    },
}

pub enum Event {
    Opened(std::result::Result<LoadedJson, Failure>),
    Validated(std::result::Result<JsonSummary, Failure>),
    Ran(std::result::Result<JsonOutcome, Failure>),
}

pub fn execute(command: Command, events: &mpsc::Sender<Outgoing>) {
    let send = |event| {
        let _ = events.send(Outgoing::Json(event));
    };
    let result = match command {
        Command::Open(language) => {
            let file = futures_lite::future::block_on(
                rfd::AsyncFileDialog::new()
                    .set_title(language.text("Open a JSON document", "打开 JSON 文档"))
                    .add_filter("JSON", json::EXTENSIONS)
                    .add_filter(language.text("All files", "所有文件"), &["*"])
                    .pick_file(),
            );
            let Some(file) = file else {
                return finish(events, Ok(Message::Cancelled));
            };
            send(Event::Opened(
                json::load(file.path()).map_err(Failure::json),
            ));
            return;
        }
        Command::Load(path) => {
            send(Event::Opened(json::load(&path).map_err(Failure::json)));
            return;
        }
        Command::Validate { input, mode } => {
            send(Event::Validated(
                json::validate(&input, mode).map_err(Failure::json),
            ));
            return;
        }
        Command::Run {
            input,
            filter,
            layout,
            mode,
        } => {
            send(Event::Ran(
                json::run(&input, &filter, layout, mode).map_err(Failure::json),
            ));
            return;
        }
        Command::Export {
            language,
            source,
            name,
            text,
        } => export(language, source, name, text).map_err(Failure::json),
    };
    finish(events, result);
}

fn finish(events: &mpsc::Sender<Outgoing>, result: std::result::Result<Message, Failure>) {
    let _ = events.send(Outgoing::Finished(result));
}

fn export(
    language: Language,
    source: Option<PathBuf>,
    name: String,
    text: String,
) -> Result<Message> {
    let selected = futures_lite::future::block_on(
        rfd::AsyncFileDialog::new()
            .set_title(language.text("Export JSON", "导出 JSON"))
            .set_file_name(&name)
            .add_filter("JSON", &["json"])
            .save_file(),
    );
    let Some(file) = selected else {
        return Ok(Message::Cancelled);
    };
    // Do not silently change an extension after the native overwrite prompt.
    json::export(source.as_deref(), file.path(), &text)?;
    Ok(Message::Saved(file.path().to_owned()))
}
