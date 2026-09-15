//! JSON workbench: the editor's document, its validation clock, and jq runs.
use super::{Shell, kib};
use crate::{
    AppWindow, JsonState,
    locale::{Failure, Language, Message},
    worker::{Command, json::Command as Job, json::Event},
};
use handybox_core::tools::json::{
    self, JsonOutcome, JsonSummary, Layout, Limit, MAX_OUTPUTS, Mode, TIME_BUDGET, ValueKind,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// Long enough that typing does not queue a validation per keystroke, short
/// enough that a pause feels like an answer.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// An edit timestamp the debounce is already past: validate on the next tick.
fn overdue() -> Instant {
    Instant::now()
        .checked_sub(DEBOUNCE)
        .unwrap_or_else(Instant::now)
}

#[derive(Default)]
enum Status {
    #[default]
    Idle,
    Valid(JsonSummary),
    Invalid(Failure),
}

#[derive(Default)]
struct State {
    source: Option<PathBuf>,
    status: Status,
    result: Option<JsonOutcome>,
    truncated: bool,
    /// When the document last changed, while a validation is still owed.
    edited: Option<Instant>,
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
        let view = ui.global::<JsonState>();

        let controller = self.clone();
        view.on_edited(move || controller.state.borrow_mut().edited = Some(Instant::now()));
        // Strictness changes what the same document means, so re-check it.
        let controller = self.clone();
        view.on_strict_toggled(move || controller.state.borrow_mut().edited = Some(overdue()));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_run(move || controller.run(&bound, Layout::Pretty));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_minify(move || controller.run(&bound, Layout::Compact));
        let bound = shell.clone();
        view.on_open(move || {
            bound.submit(Command::Json(Job::Open(bound.language())), Message::Picking);
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_sample(move || controller.replace_document(&bound, json::SAMPLE));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_clear(move || controller.replace_document(&bound, ""));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy(move || {
            if let Some(result) = &controller.state.borrow().result {
                bound.submit(Command::Copy(result.text.clone()), Message::Copying);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_save(move || {
            let state = controller.state.borrow();
            if let Some(result) = &state.result {
                bound.submit(
                    Command::Json(Job::Export {
                        language: bound.language(),
                        source: state.source.clone(),
                        name: controller.export_name(&state),
                        text: result.text.clone(),
                    }),
                    Message::Saving,
                );
            }
        });
    }

    /// The switch beside the status line: jq's stream reading, or one document.
    fn mode(&self) -> Mode {
        match self.ui.upgrade() {
            Some(ui) if ui.global::<JsonState>().get_strict() => Mode::Single,
            _ => Mode::Stream,
        }
    }

    /// Open a path the user already chose, from a drop or the command line.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("json");
        shell.submit(Command::Json(Job::Load(path)), Message::Reading);
    }

    /// Validation is owed by the clock, not by an action: it never takes the
    /// busy state, and it waits its turn when a real operation is in flight.
    pub fn tick(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let due = matches!(self.state.borrow().edited, Some(at) if at.elapsed() >= DEBOUNCE);
        if !due {
            return;
        }
        let input = ui.global::<JsonState>().get_input();
        if input.trim().is_empty() {
            let mut state = self.state.borrow_mut();
            state.edited = None;
            state.status = Status::Idle;
            drop(state);
            self.refresh(shell.language());
            return;
        }
        let command = Job::Validate {
            input: input.to_string(),
            mode: self.mode(),
        };
        if shell.dispatch(Command::Json(command)) {
            self.state.borrow_mut().edited = None;
        }
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<JsonState>();
        match event {
            Event::Opened(Ok(document)) => {
                view.set_input(document.text.into());
                view.set_source(
                    document
                        .source
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                );
                {
                    let mut state = self.state.borrow_mut();
                    state.source = Some(document.source);
                    // Opening replaces the document, so the result no longer
                    // belongs to what is on screen.
                    state.result = None;
                    state.edited = Some(overdue());
                }
                view.set_has_result(false);
                view.set_result_lines(ModelRc::default());
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Opened(Err(failure)) => shell.finish(Err(failure)),
            Event::Validated(outcome) => {
                self.state.borrow_mut().status = match outcome {
                    Ok(summary) => Status::Valid(summary),
                    Err(failure) => Status::Invalid(failure),
                };
                self.refresh(shell.language());
            }
            Event::Ran(Ok(outcome)) => {
                let (preview, truncated) = outcome.preview();
                view.set_result_lines(ModelRc::new(VecModel::from(
                    preview.lines().map(SharedString::from).collect::<Vec<_>>(),
                )));
                view.set_has_result(true);
                view.set_running(false);
                let empty = outcome.outputs == 0;
                {
                    let mut state = self.state.borrow_mut();
                    state.truncated = truncated;
                    state.result = Some(outcome);
                    // A run that parsed proves the document is valid; let the
                    // status line catch up instead of contradicting it.
                    if matches!(state.status, Status::Invalid(_)) {
                        state.edited = Some(overdue());
                    }
                }
                self.refresh(shell.language());
                shell.finish(Ok(if empty {
                    Message::NoOutput
                } else {
                    Message::Ready
                }));
            }
            Event::Ran(Err(failure)) => {
                view.set_running(false);
                // An input error is about the document, so it also belongs on
                // the editor's own status line, not only in a toast.
                if failure.about_input() {
                    self.state.borrow_mut().status = Status::Invalid(failure.clone());
                    self.refresh(shell.language());
                }
                shell.finish(Err(failure));
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<JsonState>();
        let state = self.state.borrow();
        view.set_status_error(matches!(state.status, Status::Invalid(_)));
        view.set_status(match &state.status {
            Status::Idle => SharedString::new(),
            Status::Valid(summary) => summarize(summary, lang).into(),
            Status::Invalid(failure) => failure.render(lang).into(),
        });
        view.set_stats(match &state.result {
            None => SharedString::new(),
            Some(result) => stats(result, state.truncated, lang).into(),
        });
    }

    fn run(&self, shell: &Shell, layout: Layout) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<JsonState>();
        // `submit` refuses while another operation runs; do not claim otherwise.
        let accepted = shell.submit(
            Command::Json(Job::Run {
                input: view.get_input().to_string(),
                filter: view.get_filter().to_string(),
                layout,
                mode: self.mode(),
            }),
            Message::Running,
        );
        view.set_running(accepted);
    }

    /// Replace the editor's contents from the application side: the sample, a
    /// reset, or a document opened from disk.
    fn replace_document(&self, shell: &Shell, text: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<JsonState>();
        view.set_input(text.into());
        view.set_source(SharedString::new());
        view.set_has_result(false);
        view.set_result_lines(ModelRc::default());
        {
            let mut state = self.state.borrow_mut();
            state.source = None;
            state.result = None;
            state.status = Status::Idle;
            state.edited = Some(overdue());
        }
        self.refresh(shell.language());
    }

    fn export_name(&self, state: &State) -> String {
        match &state.source {
            // Never suggest the document that was opened: exporting onto it is
            // refused, and the native dialog would offer it by default.
            Some(source) => format!(
                "{}-result.json",
                source.file_stem().unwrap_or_default().to_string_lossy()
            ),
            None => "result.json".into(),
        }
    }
}

fn summarize(summary: &JsonSummary, lang: Language) -> String {
    // Several top-level values are not one JSON document, so do not call them
    // valid JSON: they are a stream, which is what jq reads and what this tool
    // accepts. One value gets the plain verdict people expect.
    if summary.kind == ValueKind::Stream {
        return format!(
            "{} · {} {} · {}",
            lang.text("JSON stream", "JSON 数据流"),
            summary.values,
            lang.text("values", "个值"),
            kib(summary.bytes)
        );
    }
    let shape = match summary.kind {
        ValueKind::Object => format!(
            "{} {}",
            summary.entries,
            lang.text("object members", "个成员的对象")
        ),
        ValueKind::Array => format!("{} {}", summary.entries, lang.text("items", "个元素的数组")),
        ValueKind::String => lang.text("string", "字符串").into(),
        ValueKind::Number => lang.text("number", "数字").into(),
        ValueKind::Boolean => lang.text("boolean", "布尔值").into(),
        ValueKind::Null => lang.text("null", "null").into(),
        ValueKind::Stream => unreachable!("handled above"),
    };
    format!(
        "{} · {shape} · {}",
        lang.text("Valid JSON", "JSON 有效"),
        kib(summary.bytes)
    )
}

fn stats(result: &JsonOutcome, truncated: bool, lang: Language) -> String {
    let limit = match result.limit {
        Some(Limit::Outputs) => lang
            .text(
                "  ·  Stopped at the first {n} outputs.",
                "  ·  已在前 {n} 条输出处停止。",
            )
            .replace("{n}", &MAX_OUTPUTS.to_string()),
        Some(Limit::Time) => lang
            .text(
                "  ·  Stopped after {n} seconds.",
                "  ·  已在 {n} 秒后停止。",
            )
            .replace("{n}", &TIME_BUDGET.as_secs().to_string()),
        None => String::new(),
    };
    format!(
        "{} {} · {} · {} ms{limit}{}",
        result.outputs,
        lang.text("outputs", "条输出"),
        kib(result.bytes),
        result.elapsed.as_millis(),
        if truncated {
            lang.text(
                "  ·  Preview limited to 200k characters. Copy and export include everything.",
                "  ·  仅预览前 20 万字符，复制和导出保留全文。",
            )
        } else {
            ""
        }
    )
}
