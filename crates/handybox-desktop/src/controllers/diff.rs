//! Text diff: two editors, their files, and the comparison between them.
//!
//! The comparison is live. Nothing here is submitted as an operation: an edit
//! marks the result as owed, and the shell's tick dispatches one run once the
//! typing stops, the same way the workbench validates what is in its editor.
//! The retained outcome is the engine's own value, never rendered text, so a
//! language change re-renders the summary and the gaps in place.
use super::Shell;
use crate::{
    AppWindow, DiffRow, DiffState,
    locale::{Failure, Language, Message},
    worker::{Command, diff::Command as Job, diff::Event, diff::Side, diff::Text},
};
use handybox_core::tools::diff::{
    CHANGED, Change, DiffOutcome, Limit, MAX_ROWS, ORIGINAL, Options, Row, TIME_BUDGET,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// Long enough that typing does not queue a comparison per keystroke, short
/// enough that a pause feels like an answer.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// An edit timestamp the debounce is already past: compare on the next tick.
fn overdue() -> Instant {
    Instant::now()
        .checked_sub(DEBOUNCE)
        .unwrap_or_else(Instant::now)
}

/// What the last comparison left behind. The failure is kept rather than
/// rendered, like the outcome beside it, so a language change re-states it.
#[derive(Default)]
enum Status {
    #[default]
    Idle,
    Compared(DiffOutcome),
    /// Something about the two texts themselves, said under the panel.
    Invalid(Failure),
}

#[derive(Default)]
struct State {
    left: Option<PathBuf>,
    right: Option<PathBuf>,
    status: Status,
    /// When the texts or the options last changed, while a run is still owed.
    edited: Option<Instant>,
}

impl State {
    /// The file behind a side, if that side came from one.
    fn source(&self, side: Side) -> &Option<PathBuf> {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// The comparison on screen, if the last one produced one.
    fn outcome(&self) -> Option<&DiffOutcome> {
        match &self.status {
            Status::Compared(outcome) => Some(outcome),
            _ => None,
        }
    }

    /// Both files, so that exporting can refuse to replace either of them.
    fn sources(&self) -> Vec<PathBuf> {
        [&self.left, &self.right]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
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
        let view = ui.global::<DiffState>();

        let controller = self.clone();
        view.on_edited(move || controller.state.borrow_mut().edited = Some(Instant::now()));
        // An option changes what the same two texts mean, so re-compare them
        // without waiting out a typing pause that already ended.
        let controller = self.clone();
        view.on_options_changed(move || controller.state.borrow_mut().edited = Some(overdue()));
        let bound = shell.clone();
        view.on_open_left(move || {
            bound.submit(
                Command::Diff(Job::Open(bound.language(), Side::Left)),
                Message::Picking,
            );
        });
        let bound = shell.clone();
        view.on_open_right(move || {
            bound.submit(
                Command::Diff(Job::Open(bound.language(), Side::Right)),
                Message::Picking,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_clear_left(move || controller.clear(&bound, Side::Left));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_clear_right(move || controller.clear(&bound, Side::Right));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_swap(move || controller.swap(&bound));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy(move || {
            if let Some(outcome) = controller.state.borrow().outcome() {
                if !outcome.unified.is_empty() {
                    bound.submit(Command::Copy(outcome.unified.clone()), Message::Copying);
                }
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_save(move || {
            let state = controller.state.borrow();
            if let Some(outcome) = state.outcome() {
                if outcome.unified.is_empty() {
                    return;
                }
                bound.submit(
                    Command::Diff(Job::Export {
                        language: bound.language(),
                        sources: state.sources(),
                        name: export_name(&state),
                        text: outcome.unified.clone(),
                    }),
                    Message::Saving,
                );
            }
        });
    }

    /// Open a path the user already chose, from a drop or the command line. It
    /// fills the side that is still empty, so dropping two files in a row sets
    /// up a comparison; once both sides are taken, a further file replaces the
    /// changed one, which is the side that keeps moving.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("diff");
        let side = match self.ui.upgrade() {
            Some(ui) if ui.global::<DiffState>().get_left().is_empty() => Side::Left,
            _ => Side::Right,
        };
        shell.submit(Command::Diff(Job::Load(side, path)), Message::Reading);
    }

    /// Two paths named together: the comparison the user already described.
    pub fn open_pair(&self, shell: &Shell, left: PathBuf, right: PathBuf) {
        shell.navigate("diff");
        shell.submit(Command::Diff(Job::LoadPair(left, right)), Message::Reading);
    }

    /// A comparison is owed by the clock, not by an action: it never takes the
    /// busy state, and it waits its turn when a real operation is in flight.
    pub fn tick(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let due = matches!(self.state.borrow().edited, Some(at) if at.elapsed() >= DEBOUNCE);
        if !due {
            return;
        }
        let view = ui.global::<DiffState>();
        let (left, right) = (view.get_left(), view.get_right());
        if left.is_empty() && right.is_empty() {
            let mut state = self.state.borrow_mut();
            state.edited = None;
            state.status = Status::Idle;
            drop(state);
            self.refresh(shell.language());
            return;
        }
        let state = self.state.borrow();
        let command = Job::Compare {
            left: Text {
                name: name(&state.left, ORIGINAL),
                text: left.to_string(),
            },
            right: Text {
                name: name(&state.right, CHANGED),
                text: right.to_string(),
            },
            options: Options {
                ignore_whitespace: view.get_ignore_whitespace(),
                changes_only: view.get_changes_only(),
            },
        };
        drop(state);
        if shell.dispatch(Command::Diff(command)) {
            self.state.borrow_mut().edited = None;
            view.set_comparing(true);
        }
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DiffState>();
        match event {
            Event::Opened(side, Ok(document)) => {
                let filename = document
                    .source
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                match side {
                    Side::Left => {
                        view.set_left(document.text.into());
                        view.set_left_source(filename.into());
                        self.state.borrow_mut().left = Some(document.source);
                    }
                    Side::Right => {
                        view.set_right(document.text.into());
                        view.set_right_source(filename.into());
                        self.state.borrow_mut().right = Some(document.source);
                    }
                }
                self.invalidate();
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Opened(_, Err(failure)) => shell.finish(Err(failure)),
            Event::Compared(Ok(outcome)) => {
                view.set_comparing(false);
                self.state.borrow_mut().status = Status::Compared(outcome);
                self.refresh(shell.language());
            }
            Event::Compared(Err(failure)) => {
                view.set_comparing(false);
                // An input error is about the two texts, so it belongs on the
                // line under the panel, where it survives a language change.
                // Anything else is about the run, and that is a toast.
                let about_input = failure.about_input();
                self.state.borrow_mut().status = if about_input {
                    Status::Invalid(failure.clone())
                } else {
                    Status::Idle
                };
                self.refresh(shell.language());
                if !about_input {
                    shell.notify(Message::Error(failure));
                }
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DiffState>();
        let state = self.state.borrow();
        let outcome = match &state.status {
            Status::Compared(outcome) => outcome,
            status => {
                view.set_has_result(false);
                view.set_has_changes(false);
                view.set_rows(ModelRc::default());
                view.set_stats(SharedString::new());
                let invalid = matches!(status, Status::Invalid(_));
                view.set_status(match status {
                    Status::Invalid(failure) => failure.render(lang).into(),
                    _ => SharedString::new(),
                });
                view.set_status_tone(if invalid { 2 } else { 0 });
                return;
            }
        };
        view.set_rows(ModelRc::new(VecModel::from(
            outcome
                .rows
                .iter()
                .map(|row| present(row, lang))
                .collect::<Vec<_>>(),
        )));
        view.set_has_result(true);
        view.set_has_changes(!outcome.unified.is_empty());
        view.set_stats(stats(outcome, lang).into());
        view.set_status(summarize(outcome, lang).into());
        view.set_status_tone(if outcome.identical() { 1 } else { 0 });
    }

    /// Empty one side, keeping the other and the file behind it.
    fn clear(&self, shell: &Shell, side: Side) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DiffState>();
        match side {
            Side::Left => {
                view.set_left(SharedString::new());
                view.set_left_source(SharedString::new());
                self.state.borrow_mut().left = None;
            }
            Side::Right => {
                view.set_right(SharedString::new());
                view.set_right_source(SharedString::new());
                self.state.borrow_mut().right = None;
            }
        }
        self.invalidate();
        self.refresh(shell.language());
    }

    /// Read the comparison the other way round. Everything a side owns travels
    /// with it, including the file it came from and the name in the diff header.
    fn swap(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<DiffState>();
        let (left, right) = (view.get_left(), view.get_right());
        view.set_left(right);
        view.set_right(left);
        let (left_source, right_source) = (view.get_left_source(), view.get_right_source());
        view.set_left_source(right_source);
        view.set_right_source(left_source);
        {
            let state = &mut *self.state.borrow_mut();
            std::mem::swap(&mut state.left, &mut state.right);
        }
        self.invalidate();
        self.refresh(shell.language());
    }

    /// Drop the result and ask for a new one: what is on screen no longer
    /// belongs to the texts beside it.
    fn invalidate(&self) {
        let mut state = self.state.borrow_mut();
        state.status = Status::Idle;
        state.edited = Some(overdue());
    }
}

/// What a side is called in the diff header: its file name, or the neutral
/// label for text that was typed rather than opened.
fn name(source: &Option<PathBuf>, fallback: &str) -> String {
    match source {
        Some(path) => path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        None => fallback.to_owned(),
    }
}

fn export_name(state: &State) -> String {
    match state.source(Side::Right).as_ref().or(state.left.as_ref()) {
        Some(source) => format!(
            "{}.diff",
            source.file_stem().unwrap_or_default().to_string_lossy()
        ),
        None => "changes.diff".into(),
    }
}

/// One engine row as the panel shows it. `kind` is 0 unchanged, 1 added,
/// 2 removed, 3 a gap — the only place the sentence around a gap is written.
fn present(row: &Row, lang: Language) -> DiffRow {
    match row {
        Row::Gap(skipped) => DiffRow {
            kind: 3,
            old_line: SharedString::new(),
            new_line: SharedString::new(),
            text: lang
                .text("…  {n} unchanged lines", "…  {n} 行未变更")
                .replace("{n}", &skipped.to_string())
                .into(),
        },
        Row::Line {
            change,
            old,
            new,
            text,
        } => DiffRow {
            kind: match change {
                Change::Equal => 0,
                Change::Insert => 1,
                Change::Delete => 2,
            },
            old_line: number(*old),
            new_line: number(*new),
            text: text.into(),
        },
    }
}

fn number(line: Option<usize>) -> SharedString {
    line.map(|line| line.to_string()).unwrap_or_default().into()
}

fn stats(outcome: &DiffOutcome, lang: Language) -> String {
    let limit = match outcome.limit {
        Some(Limit::Rows) => lang
            .text(
                "  ·  Showing the first {n} lines. Copy and export include everything.",
                "  ·  仅显示前 {n} 行，复制和导出保留全部内容。",
            )
            .replace("{n}", &MAX_ROWS.to_string()),
        Some(Limit::Time) => lang
            .text(
                "  ·  Stopped looking for a smaller diff after {n} seconds.",
                "  ·  已在 {n} 秒后停止寻找更小的差异。",
            )
            .replace("{n}", &TIME_BUDGET.as_secs().to_string()),
        None => String::new(),
    };
    format!(
        "{} {} · {} {} · {} ms{limit}",
        outcome.old_lines,
        lang.text("lines", "行"),
        outcome.new_lines,
        lang.text("lines", "行"),
        outcome.elapsed.as_millis()
    )
}

/// The one line people look at after a comparison.
fn summarize(outcome: &DiffOutcome, lang: Language) -> String {
    if outcome.identical() {
        return lang
            .text("The two sides are identical.", "两侧内容完全一致。")
            .to_owned();
    }
    format!(
        "+{} {} · -{} {} · {} {} · {}% {}",
        outcome.added,
        lang.text("added", "行新增"),
        outcome.removed,
        lang.text("removed", "行删除"),
        outcome.hunks,
        if outcome.hunks == 1 {
            lang.text("change", "处改动")
        } else {
            lang.text("changes", "处改动")
        },
        (outcome.similarity * 100.0).round() as i32,
        lang.text("similar", "相似"),
    )
}
