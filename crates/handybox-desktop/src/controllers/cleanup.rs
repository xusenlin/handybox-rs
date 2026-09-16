//! Disk cleanup: read a folder, choose from what it found, and move only that
//! to the trash.
//!
//! The one tool here that changes files it was not handed, so the order is
//! fixed: scan, look, select, confirm, remove. Nothing is selected by default,
//! the helper that selects for you always leaves a copy, and the plan shown in
//! the confirmation is the same value that is sent to the worker — the number
//! on the button cannot drift from the files that would go.
//!
//! What is retained is the engine's own outcome, never rendered text, so a
//! language change re-states every line in place and the selection survives it.
use super::Shell;
use crate::{
    AppWindow, CleanupRow, CleanupState,
    locale::{Failure, Language, Message, size},
    worker::{Command, cleanup::Command as Job, cleanup::Event},
};
use handybox_core::tools::cleanup::{self, Limit, Mode, Plan, ScanOutcome};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct State {
    root: Option<PathBuf>,
    mode: Mode,
    outcome: Option<ScanOutcome>,
    /// Indices into the outcome's entries. Empty until someone chooses.
    selected: BTreeSet<usize>,
    /// The checked plan behind an armed confirmation. Holding it is what keeps
    /// the confirmation and the removal talking about the same files.
    confirming: Option<Plan>,
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
    /// Shared with the scan that is running. Raising it is how a question that
    /// is no longer being asked stops being answered.
    cancel: Arc<AtomicBool>,
}

impl Controller {
    pub fn new(ui: &AppWindow) -> Self {
        Self {
            ui: ui.as_weak(),
            state: Rc::new(RefCell::new(State::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn bind(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CleanupState>();

        let bound = shell.clone();
        view.on_choose(move || {
            bound.submit(
                Command::Cleanup(Job::Choose(bound.language())),
                Message::Picking,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_scan(move || {
            controller.start(&bound);
        });
        // A result belongs to the mode it was read with, so the old one goes
        // and the folder is read again for the new one. Asking a second time is
        // what switching means here: the first scan started by itself, and a
        // page that emptied its list and then waited would look broken.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_mode_selected(move |mode| {
            controller.state.borrow_mut().mode = match mode {
                1 => Mode::Empty,
                2 => Mode::Large,
                _ => Mode::Duplicates,
            };
            controller.discard();
            controller.refresh(bound.language());
            // Nothing to read until a folder has been chosen; `start` says so
            // by doing nothing. If it is refused, a scan of the old question is
            // still running: stop it, and its own event starts this one.
            if !controller.start(&bound) {
                controller.cancel.store(true, Ordering::Relaxed);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_toggle(move |index| {
            if index >= 0 {
                let mut state = controller.state.borrow_mut();
                let index = index as usize;
                if !state.selected.remove(&index) {
                    state.selected.insert(index);
                }
                // A selection that changed is not the one that was confirmed.
                state.confirming = None;
            }
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_select_extra(move || {
            controller.select_extra();
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_clear_selection(move || {
            {
                let mut state = controller.state.borrow_mut();
                state.selected.clear();
                state.confirming = None;
            }
            controller.refresh(bound.language());
        });
        // Arming the confirmation is where the selection is checked. What it
        // shows is built from the plan itself, so the count, the size and the
        // files that would go all come from one value.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_ask(move || {
            let checked = {
                let state = controller.state.borrow();
                match &state.outcome {
                    Some(outcome) => {
                        cleanup::plan(outcome, &state.selected.iter().copied().collect::<Vec<_>>())
                    }
                    None => return,
                }
            };
            match checked {
                Ok(plan) => {
                    controller.state.borrow_mut().confirming = Some(plan);
                    controller.refresh(bound.language());
                }
                Err(error) => bound.notify(Message::Error(Failure::cleanup(error))),
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_cancel(move || {
            controller.state.borrow_mut().confirming = None;
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_confirm(move || {
            let (root, plan) = {
                let state = controller.state.borrow();
                match (&state.root, &state.confirming) {
                    (Some(root), Some(plan)) => (root.clone(), plan.clone()),
                    _ => return,
                }
            };
            if bound.submit(
                Command::Cleanup(Job::Remove { root, plan }),
                Message::Removing,
            ) {
                controller.state.borrow_mut().confirming = None;
                controller.refresh(bound.language());
            }
        });
    }

    /// Adopt a folder the user already chose, from a drop or the command line,
    /// and read it: choosing a folder for this tool is asking what is in it.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("cleanup");
        self.adopt(shell, path);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CleanupState>();
        match event {
            Event::Chosen(path) => {
                // The picker's operation ends here; the scan that follows takes
                // the busy state on its own.
                shell.finish(Ok(Message::Ready));
                self.adopt(shell, path);
            }
            // A scan that was stopped answers a question nobody is asking any
            // more. Its rows are dropped and the one that replaced it starts.
            Event::Scanned(Ok(outcome)) if outcome.stopped == Some(Limit::Cancelled) => {
                shell.finish(Ok(Message::Ready));
                if !self.start(shell) {
                    view.set_working(false);
                }
            }
            Event::Scanned(Ok(outcome)) => {
                view.set_working(false);
                let empty = outcome.entries.is_empty();
                {
                    let mut state = self.state.borrow_mut();
                    state.outcome = Some(outcome);
                    state.selected.clear();
                    state.confirming = None;
                }
                self.refresh(shell.language());
                shell.finish(Ok(if empty {
                    Message::NothingFound
                } else {
                    Message::Ready
                }));
            }
            Event::Scanned(Err(failure)) => {
                view.set_working(false);
                self.discard();
                self.refresh(shell.language());
                shell.finish(Err(failure));
            }
            Event::Removed(removal) => {
                // What is in the trash is gone from the folder, so it goes from
                // the result too — without reading the whole folder again.
                self.forget(&removal.gone);
                self.refresh(shell.language());
                shell.finish(Ok(Message::Trashed {
                    files: removal.gone.len(),
                    freed: removal.freed,
                    skipped: removal.skipped + removal.failed,
                }));
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<CleanupState>();
        let state = self.state.borrow();
        view.set_mode(match state.mode {
            Mode::Duplicates => 0,
            Mode::Empty => 1,
            Mode::Large => 2,
        });
        view.set_folder(match &state.root {
            Some(root) => root.display().to_string().into(),
            None => SharedString::new(),
        });
        view.set_detail(SharedString::from(if state.root.is_some() {
            lang.text(
                "Reads only. Nothing is removed until you choose it and confirm.",
                "只读取，不改动。只有你选中并确认后，才会移动文件。",
            )
        } else {
            lang.text(
                "Pick a folder to look through, or drop one on the window",
                "选择要查看的文件夹，或把文件夹拖到窗口上",
            )
        }));
        view.set_can_scan(state.root.is_some());

        let Some(outcome) = &state.outcome else {
            view.set_rows(ModelRc::default());
            view.set_has_result(false);
            view.set_summary(SharedString::new());
            view.set_selection(SharedString::new());
            view.set_has_selection(false);
            view.set_confirming(false);
            view.set_confirm_text(SharedString::new());
            let (line, tone) = status(None, state.mode, lang);
            view.set_status(line.into());
            view.set_status_tone(tone);
            return;
        };
        view.set_rows(ModelRc::new(VecModel::from(rows(
            outcome,
            &state.selected,
            lang,
        ))));
        view.set_has_result(!outcome.entries.is_empty());
        view.set_summary(summary(outcome, lang).into());
        let chosen: u64 = state
            .selected
            .iter()
            .filter_map(|index| outcome.entries.get(*index))
            .map(|entry| entry.size)
            .sum();
        view.set_has_selection(!state.selected.is_empty());
        view.set_selection(if state.selected.is_empty() {
            SharedString::new()
        } else {
            format!(
                "{} {} {}  ·  {}",
                lang.text("Selected", "已选"),
                state.selected.len(),
                items(state.selected.len(), lang),
                size(chosen)
            )
            .into()
        });
        // The one place that names what is about to happen, built from the plan
        // that would be carried out rather than from the selection beside it.
        view.set_confirming(state.confirming.is_some());
        view.set_confirm_text(match &state.confirming {
            Some(plan) => confirmation(plan, lang).into(),
            None => SharedString::new(),
        });
        let (line, tone) = status(Some(outcome), state.mode, lang);
        view.set_status(line.into());
        view.set_status_tone(tone);
    }

    /// Take a folder as the input and read it. A scan is an operation like any
    /// other: it takes the busy state and reports through the worker.
    fn adopt(&self, shell: &Shell, path: PathBuf) {
        {
            let mut state = self.state.borrow_mut();
            state.root = Some(path);
            state.outcome = None;
            state.selected.clear();
            state.confirming = None;
        }
        self.refresh(shell.language());
        self.start(shell);
    }

    /// Read the folder for the question the page is currently asking. Returns
    /// whether the worker took it: `submit` refuses while another operation is
    /// running, and the caller decides what that means.
    fn start(&self, shell: &Shell) -> bool {
        let Some(ui) = self.ui.upgrade() else {
            return false;
        };
        let (root, mode) = {
            let state = self.state.borrow();
            match &state.root {
                Some(root) => (root.clone(), state.mode),
                None => return false,
            }
        };
        self.cancel.store(false, Ordering::Relaxed);
        let accepted = shell.submit(
            Command::Cleanup(Job::Scan {
                root,
                mode,
                cancel: self.cancel.clone(),
            }),
            Message::Searching,
        );
        // Do not claim to be working when nothing was taken; a scan that is
        // still running keeps the bar it already has.
        if accepted {
            ui.global::<CleanupState>().set_working(true);
        }
        accepted
    }

    /// Put the page back to having no result, keeping the folder.
    fn discard(&self) {
        let mut state = self.state.borrow_mut();
        state.outcome = None;
        state.selected.clear();
        state.confirming = None;
    }

    /// Drop what is now in the trash from the result. A duplicate group that is
    /// left with a single copy is no longer a finding, so it goes with them.
    fn forget(&self, gone: &[PathBuf]) {
        let mut state = self.state.borrow_mut();
        state.selected.clear();
        state.confirming = None;
        let Some(outcome) = state.outcome.as_mut() else {
            return;
        };
        outcome.entries.retain(|entry| !gone.contains(&entry.path));
        if outcome.mode == Mode::Duplicates {
            let mut left: std::collections::HashMap<Option<u32>, usize> = Default::default();
            for entry in &outcome.entries {
                *left.entry(entry.group).or_default() += 1;
            }
            outcome
                .entries
                .retain(|entry| left.get(&entry.group).copied().unwrap_or(0) > 1);
            outcome.groups = left.values().filter(|count| **count > 1).count() as u32;
        }
        outcome.reclaimable = reclaimable(outcome);
    }

    /// The selection people actually want: every extra copy, or everything that
    /// is empty. Never offered for the biggest files — those are not rubbish,
    /// they are simply large, and only a person can say which may go.
    fn select_extra(&self) {
        let mut state = self.state.borrow_mut();
        state.confirming = None;
        let Some(outcome) = &state.outcome else {
            return;
        };
        let chosen: BTreeSet<usize> = match outcome.mode {
            Mode::Duplicates => {
                let mut seen: BTreeSet<u32> = BTreeSet::new();
                outcome
                    .entries
                    .iter()
                    .enumerate()
                    // The first copy of each group stays; every later one is
                    // the extra this tool exists to remove.
                    .filter(|(_, entry)| match entry.group {
                        Some(group) => !seen.insert(group),
                        None => false,
                    })
                    .map(|(index, _)| index)
                    .collect()
            }
            Mode::Empty => (0..outcome.entries.len()).collect(),
            Mode::Large => return,
        };
        state.selected = chosen;
    }
}

/// What deleting everything worth deleting would still free.
fn reclaimable(outcome: &ScanOutcome) -> u64 {
    match outcome.mode {
        Mode::Duplicates => {
            let mut per_group: std::collections::HashMap<Option<u32>, (u64, u64)> =
                Default::default();
            for entry in &outcome.entries {
                let slot = per_group.entry(entry.group).or_insert((entry.size, 0));
                slot.1 += 1;
            }
            per_group
                .values()
                .map(|(size, count)| size * count.saturating_sub(1))
                .sum()
        }
        Mode::Empty => 0,
        Mode::Large => outcome.entries.iter().map(|entry| entry.size).sum(),
    }
}

/// The list as rows. Duplicate groups get a heading of their own, because what
/// a row means — one copy of several — is a property of the group above it.
fn rows(outcome: &ScanOutcome, selected: &BTreeSet<usize>, lang: Language) -> Vec<CleanupRow> {
    let mut rows = Vec::with_capacity(outcome.entries.len() + outcome.groups as usize);
    let mut group = None;
    for (index, entry) in outcome.entries.iter().enumerate() {
        if entry.group.is_some() && entry.group != group {
            group = entry.group;
            let copies = outcome
                .entries
                .iter()
                .filter(|other| other.group == group)
                .count();
            rows.push(CleanupRow {
                header: true,
                label: format!(
                    "{}  ·  {} {}  ·  {}",
                    if lang.chinese() {
                        format!("第 {} 组", group.unwrap_or_default() + 1)
                    } else {
                        format!("Group {}", group.unwrap_or_default() + 1)
                    },
                    copies,
                    lang.text("copies", "份副本"),
                    size(entry.size)
                )
                .into(),
                detail: format!(
                    "{} {}",
                    lang.text("Wasting", "多占用"),
                    size(entry.size * (copies as u64 - 1))
                )
                .into(),
                selected: false,
                index: -1,
            });
        }
        rows.push(CleanupRow {
            header: false,
            // Relative to the folder that was scanned: the folder itself is
            // already named above the list, and a full path in every row would
            // push the part that differs off the end.
            label: entry
                .path
                .strip_prefix(&outcome.root)
                .unwrap_or(&entry.path)
                .display()
                .to_string()
                .into(),
            detail: if entry.directory {
                SharedString::from(lang.text("Empty folder", "空目录"))
            } else if entry.size == 0 {
                SharedString::from(lang.text("Empty file", "空文件"))
            } else {
                size(entry.size).into()
            },
            selected: selected.contains(&index),
            index: index as i32,
        });
    }
    rows
}

fn summary(outcome: &ScanOutcome, lang: Language) -> String {
    let files = outcome
        .entries
        .iter()
        .filter(|entry| !entry.directory)
        .count();
    let mut parts = Vec::new();
    match outcome.mode {
        Mode::Duplicates => {
            parts.push(format!(
                "{} {}",
                outcome.groups,
                if outcome.groups == 1 {
                    lang.text("group", "组重复")
                } else {
                    lang.text("groups", "组重复")
                }
            ));
            parts.push(format!("{files} {}", items(files, lang)));
            parts.push(format!(
                "{} {}",
                lang.text("reclaimable", "可回收"),
                size(outcome.reclaimable)
            ));
        }
        Mode::Empty => {
            let folders = outcome.entries.len() - files;
            parts.push(format!("{files} {}", lang.text("empty files", "个空文件")));
            parts.push(format!(
                "{folders} {}",
                lang.text("empty folders", "个空目录")
            ));
        }
        Mode::Large => {
            parts.push(format!("{files} {}", items(files, lang)));
            parts.push(format!(
                "{} {}",
                lang.text("holding", "共占用"),
                size(outcome.reclaimable)
            ));
        }
    }
    parts.push(format!(
        "{} {} {}",
        lang.text("read", "已查看"),
        outcome.scanned,
        items(outcome.scanned, lang)
    ));
    parts.push(format!("{} ms", outcome.elapsed.as_millis()));
    parts.join("  ·  ")
}

/// The sentence under the list. Before a scan it says what the mode looks for;
/// after one it says whether the answer is the whole folder or a prefix of it.
fn status(outcome: Option<&ScanOutcome>, mode: Mode, lang: Language) -> (String, i32) {
    let Some(outcome) = outcome else {
        return (
            match mode {
                Mode::Duplicates => lang.text(
                    "Files are compared by content: same size, then a hash of the first bytes, then the whole file.",
                    "重复判断只看内容：先比大小，再比开头的哈希，最后整份文件逐字节确认。",
                ),
                Mode::Empty => lang.text(
                    "Finds files with nothing in them, and folders with nothing under them.",
                    "查找完全为空的文件，以及下面什么都没有的目录。",
                ),
                Mode::Large => lang.text(
                    "Lists the biggest files so you can see where the room went. Nothing here is rubbish by itself.",
                    "按体积列出最大的文件，让你看清空间去了哪里。它们本身并不是垃圾。",
                ),
            }
            .to_owned(),
            0,
        );
    };
    if let Some(limit) = outcome.stopped {
        return (
            match limit {
                Limit::Files => lang.text(
                    "This folder holds more files than one scan looks at, so this is a partial answer. Scan a folder inside it for the rest.",
                    "该文件夹的文件数超过单次扫描的上限，这里只是部分结果。可以进入其中的子文件夹分别扫描。",
                ),
                Limit::Bytes => lang.text(
                    "The scan reached the amount it reads in one pass, so this is a partial answer.",
                    "本次扫描已达到单次读取上限，这里只是部分结果。",
                ),
                Limit::Time => lang.text(
                    "The scan stopped at its time limit, so this is a partial answer. A smaller folder finishes.",
                    "扫描达到时间上限后停下，这里只是部分结果。换一个更小的文件夹就能读完。",
                ),
                // Never shown: a cancelled outcome is dropped rather than read.
                Limit::Cancelled => "",
            }
            .to_owned(),
            2,
        );
    }
    if outcome.entries.is_empty() {
        return (
            match outcome.mode {
                Mode::Duplicates => lang.text(
                    "No duplicates here: every file in this folder has its own content.",
                    "没有发现重复：这个文件夹里每个文件的内容都不一样。",
                ),
                Mode::Empty => lang.text("Nothing empty here.", "这里没有空文件或空目录。"),
                Mode::Large => lang.text("This folder holds no files.", "这个文件夹里没有文件。"),
            }
            .to_owned(),
            1,
        );
    }
    (
        lang.text(
            "Nothing has been touched. What you select is moved to the system trash, where it can be put back.",
            "目前还没有改动任何文件。选中的内容会被移到系统回收站，仍然可以还原。",
        )
        .to_owned(),
        1,
    )
}

/// What the confirmation says. Deliberately concrete: how many, how large, and
/// where they go.
fn confirmation(plan: &Plan, lang: Language) -> String {
    let folders = plan
        .targets
        .iter()
        .filter(|target| target.directory)
        .count();
    let files = plan.targets.len() - folders;
    let mut what = Vec::new();
    if files > 0 {
        what.push(format!("{files} {}", items(files, lang)));
    }
    if folders > 0 {
        what.push(format!(
            "{folders} {}",
            lang.text("empty folders", "个空目录")
        ));
    }
    format!(
        "{} {}{} {}",
        lang.text("Move", "即将把"),
        what.join(lang.text(" and ", " 和 ")),
        if plan.freed > 0 {
            format!(" ({})", size(plan.freed))
        } else {
            String::new()
        },
        lang.text("to the system trash?", "移到系统回收站，确定吗？"),
    )
}

fn items(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("file", "个文件")
    } else {
        lang.text("files", "个文件")
    }
}
