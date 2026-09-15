//! Document converter: file choice, drag and drop, retained result, export.
use super::{Shell, kib};
use crate::{
    AppWindow, DocumentsState,
    locale::{Language, Message},
    worker::{Command, documents::Command as Job, documents::Event},
};
use handybox_core::tools::documents::ConvertedDocument;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

#[derive(Default)]
struct State {
    result: Option<ConvertedDocument>,
    source: Option<PathBuf>,
    truncated: bool,
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
}

impl Controller {
    pub fn new(ui: &AppWindow) -> Self {
        Self {
            ui: ui.as_weak(),
            state: Rc::new(RefCell::new(State::default())),
        }
    }

    pub fn bind(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = ui.global::<DocumentsState>();
        let bound = shell.clone();
        state.on_choose(move || {
            bound.submit(
                Command::Documents(Job::Pick(bound.language())),
                Message::Picking,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        state.on_copy(move || {
            if let Some(result) = &controller.state.borrow().result {
                bound.submit(Command::Copy(result.markdown.clone()), Message::Copying);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        state.on_save(move || {
            if let Some(result) = &controller.state.borrow().result {
                bound.submit(
                    Command::Documents(Job::Export {
                        language: bound.language(),
                        source: result.source.clone(),
                        name: result.suggested_name(),
                        markdown: result.markdown.clone(),
                    }),
                    Message::Saving,
                );
            }
        });
    }

    /// Convert a path the user already chose, from a drop or the command line.
    pub fn convert(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("documents");
        shell.submit(Command::Documents(Job::Convert(path)), Message::Reading);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DocumentsState>();
        match event {
            Event::Started(path) => {
                let mut state = self.state.borrow_mut();
                state.result = None;
                state.truncated = false;
                view.set_converting(true);
                view.set_has_result(false);
                view.set_markdown_lines(ModelRc::default());
                view.set_stats("".into());
                view.set_filename(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                );
                state.source = Some(path);
                drop(state);
                self.refresh(shell.language());
                shell.notify(Message::Converting);
            }
            Event::Converted(Ok(document)) => {
                let (preview, truncated) = document.preview();
                // One row per source line: the view only shapes what is on
                // screen, so a long document costs no more than a short one.
                view.set_markdown_lines(ModelRc::new(VecModel::from(
                    preview.lines().map(SharedString::from).collect::<Vec<_>>(),
                )));
                view.set_has_result(true);
                view.set_converting(false);
                let message = if document.markdown.trim().is_empty() {
                    Message::EmptyResult
                } else {
                    Message::Complete
                };
                {
                    let mut state = self.state.borrow_mut();
                    state.truncated = truncated;
                    state.result = Some(document);
                }
                self.refresh(shell.language());
                shell.finish(Ok(message));
            }
            Event::Converted(Err(failure)) => {
                view.set_converting(false);
                shell.finish(Err(failure));
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DocumentsState>();
        let state = self.state.borrow();
        if let Some(document) = &state.result {
            view.set_detail(
                format!(
                    "{} · {} · {} {} ms",
                    document.format,
                    kib(document.input_bytes as usize),
                    lang.text("Converted in", "转换耗时"),
                    document.elapsed.as_millis()
                )
                .into(),
            );
            view.set_stats(
                format!(
                    "{} {} · {} {} · UTF-8{}",
                    document.markdown.chars().count(),
                    lang.text("characters", "字符"),
                    document.markdown.lines().count(),
                    lang.text("lines", "行"),
                    if state.truncated {
                        lang.text(
                            "  ·  Preview limited to 200k characters. Copy and export include everything.",
                            "  ·  仅预览前 20 万字符，复制和导出保留全文。",
                        )
                    } else {
                        ""
                    }
                )
                .into(),
            );
        } else if let Some(source) = &state.source {
            view.set_detail(source.display().to_string().into());
        } else {
            view.set_detail(
                lang.text(
                    "Office, PDF, EPUB and more · Automatically detects the format",
                    "支持 Office、PDF、EPUB 等文档 · 自动识别格式",
                )
                .into(),
            );
        }
    }
}
