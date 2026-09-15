//! Barcode reader: one picture, every code in it, and where each one sits.
//!
//! There is nothing to configure about reading a code, so choosing a picture is
//! the whole interaction: the pass starts by itself and its result replaces what
//! was there. The retained outcome is the engine's own value, never rendered
//! text, so a language change re-states the summary in place — and the preview,
//! which is pixels rather than words, survives it untouched.
use super::Shell;
use crate::{
    AppWindow, CodeSymbol, CodesState,
    locale::{Language, Message},
    worker::{Command, codes::Command as Job, codes::Event},
};
use handybox_core::tools::codes::{ScanOutcome, Symbol};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

#[derive(Default)]
struct State {
    source: Option<PathBuf>,
    outcome: Option<ScanOutcome>,
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
        let view = ui.global::<CodesState>();

        let bound = shell.clone();
        view.on_choose(move || {
            bound.submit(
                Command::Codes(Job::Choose(bound.language())),
                Message::Picking,
            );
        });
        // What gets pasted somewhere else is the text itself, one code per line;
        // the format and the position are for reading here.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy(move || {
            let text = controller.texts().join("\n");
            if !text.is_empty() {
                bound.submit(Command::Copy(text), Message::Copying);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy_one(move |index| {
            if let Some(text) = controller.texts().get(index.max(0) as usize) {
                bound.submit(Command::Copy(text.clone()), Message::Copying);
            }
        });
    }

    /// Adopt a path the user already chose, from a drop or the command line.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("codes");
        self.adopt(shell, path);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CodesState>();
        match event {
            Event::Chosen(path) => {
                // The picker's operation ends here; the pass that follows takes
                // the busy state on its own.
                shell.finish(Ok(Message::Ready));
                self.adopt(shell, path);
            }
            Event::Scanned(Ok(outcome)) => {
                view.set_working(false);
                view.set_preview(preview(&outcome));
                view.set_ratio(outcome.width.max(1) as f32 / outcome.height.max(1) as f32);
                view.set_has_image(true);
                // Point at the first code, so a picture holding exactly one is
                // already highlighted when the pass finishes.
                view.set_selected(if outcome.symbols.is_empty() { -1 } else { 0 });
                self.state.borrow_mut().outcome = Some(outcome);
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Scanned(Err(failure)) => {
                view.set_working(false);
                // The file could not be read at all, so there is no picture to
                // leave on screen beside the complaint about it.
                self.reset(shell, None);
                shell.finish(Err(failure));
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CodesState>();
        let state = self.state.borrow();
        view.set_detail(match &state.source {
            Some(source) => source.display().to_string().into(),
            None => SharedString::from(
                lang.text("Screenshots, photos and scans", "支持截图、照片与扫描件"),
            ),
        });
        let Some(outcome) = &state.outcome else {
            view.set_symbols(ModelRc::default());
            view.set_has_result(false);
            view.set_dimensions(SharedString::new());
            view.set_stats(SharedString::new());
            view.set_status(SharedString::new());
            view.set_status_tone(0);
            return;
        };
        view.set_symbols(ModelRc::new(VecModel::from(
            outcome
                .symbols
                .iter()
                .enumerate()
                .map(|(index, symbol)| present(index, symbol, lang))
                .collect::<Vec<_>>(),
        )));
        view.set_has_result(!outcome.symbols.is_empty());
        view.set_dimensions(format!("{} × {}", outcome.width, outcome.height).into());
        view.set_stats(stats(outcome, lang).into());
        let (status, tone) = status(outcome, lang);
        view.set_status(status.into());
        view.set_status_tone(tone);
    }

    /// Take a picture as the input and start reading it. Recognition is an
    /// operation like any other: it takes the busy state and reports through
    /// the worker, so the page keeps its progress bar and stays navigable.
    fn adopt(&self, shell: &Shell, path: PathBuf) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CodesState>();
        view.set_filename(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
                .into(),
        );
        self.reset(shell, Some(path.clone()));
        let accepted = shell.submit(Command::Codes(Job::Scan(path)), Message::Scanning);
        // `submit` refuses while another operation runs; do not claim otherwise.
        view.set_working(accepted);
    }

    /// Put the page back to having no result. The picture goes with it: what is
    /// on screen always belongs to the file named above it.
    fn reset(&self, shell: &Shell, source: Option<PathBuf>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CodesState>();
        view.set_preview(Image::default());
        view.set_has_image(false);
        view.set_selected(-1);
        if source.is_none() {
            view.set_filename(SharedString::new());
        }
        {
            let mut state = self.state.borrow_mut();
            state.source = source;
            state.outcome = None;
        }
        self.refresh(shell.language());
    }

    /// What each code says, in the order they are listed.
    fn texts(&self) -> Vec<String> {
        match &self.state.borrow().outcome {
            Some(outcome) => outcome
                .symbols
                .iter()
                .map(|symbol| symbol.text.clone())
                .collect(),
            None => Vec::new(),
        }
    }
}

/// The decoded picture as an image the renderer can take. Built once, when the
/// pass finishes: the pixels are the engine's, and copying them again on every
/// language change would be a copy for nothing.
fn preview(outcome: &ScanOutcome) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &outcome.preview.rgba,
        outcome.preview.width,
        outcome.preview.height,
    ))
}

fn present(index: usize, symbol: &Symbol, lang: Language) -> CodeSymbol {
    let characters = symbol.text.chars().count();
    CodeSymbol {
        index: (index + 1).to_string().into(),
        format: symbol.symbology.label().into(),
        text: symbol.text.as_str().into(),
        detail: format!(
            "{characters} {}",
            if characters == 1 {
                lang.text("character", "个字符")
            } else {
                lang.text("characters", "个字符")
            }
        )
        .into(),
        x: symbol.area.x,
        y: symbol.area.y,
        width: symbol.area.width,
        height: symbol.area.height,
    }
}

fn stats(outcome: &ScanOutcome, lang: Language) -> String {
    format!(
        "{} {} · {} ms",
        outcome.symbols.len(),
        codes(outcome.symbols.len(), lang),
        outcome.elapsed.as_millis()
    )
}

/// The one line people look at after a pass. Nothing found is not an error —
/// plenty of pictures hold no code — but it is the answer they came for.
fn status(outcome: &ScanOutcome, lang: Language) -> (String, i32) {
    if outcome.symbols.is_empty() {
        return (
            lang.text(
                "No code was found in this image. A sharper or closer picture usually helps.",
                "未在该图片中找到条码。更清晰或更近的图片通常会有帮助。",
            )
            .into(),
            2,
        );
    }
    // Each symbology once, in the order it first appears.
    let mut formats: Vec<&str> = Vec::new();
    for symbol in &outcome.symbols {
        let label = symbol.symbology.label();
        if !formats.contains(&label) {
            formats.push(label);
        }
    }
    (
        format!(
            "{} {} {}: {}",
            lang.text("Found", "共识别到"),
            outcome.symbols.len(),
            codes(outcome.symbols.len(), lang),
            formats.join(" · ")
        ),
        1,
    )
}

fn codes(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("code", "个码")
    } else {
        lang.text("codes", "个码")
    }
}
