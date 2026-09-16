//! Archives: what is inside one, what would go into one, and the ticked part of
//! either that a button acts on.
//!
//! One page, two directions, and each keeps its own state. The archive that was
//! read is still there after a look at the folder being packed, because coming
//! back to a list and finding it emptied is the kind of thing that makes people
//! read a folder twice.
//!
//! Nothing here decides what is safe to write: the engine marks an entry that
//! would land outside the folder, and this refuses to let such a row be ticked.
//! The rule and the checkbox therefore cannot drift apart.
//!
//! What is retained is the engine's own values, never rendered text, so a
//! language change re-states every line in place and both selections survive it.
use super::Shell;
use crate::{
    AppWindow, ArchiveRow, ArchivesState,
    locale::{Language, Message, size},
    worker::{
        Command,
        archives::{Command as Job, Event},
    },
};
use handybox_core::tools::archives::{Kind, Listing, Plan};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Which direction the page is working in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    /// An archive is open, and what is ticked comes out of it.
    #[default]
    Open,
    /// A folder is open, and what is ticked goes into a new archive.
    Pack,
}

#[derive(Default)]
struct State {
    mode: Mode,
    listing: Option<Listing>,
    /// Shared with the job that is packing: a folder of twenty thousand files
    /// is not worth copying into a message.
    plan: Option<Arc<Plan>>,
    /// Indices into the archive's entries, and into the folder's files. Two
    /// sets, because they are selections in two different lists.
    opened: BTreeSet<usize>,
    packing: BTreeSet<usize>,
    /// What a new archive is written as. An archive being read is whatever it
    /// already is, and this is left alone.
    kind: Kind,
    /// Where a run has got to, while one is running.
    progress: Option<(usize, usize)>,
    /// The path that was chosen, shown while it is being read — before there is
    /// a listing or a plan to take the name from.
    pending: Option<PathBuf>,
}

impl State {
    /// The selection of the side that is showing.
    fn ticked(&self) -> &BTreeSet<usize> {
        match self.mode {
            Mode::Open => &self.opened,
            Mode::Pack => &self.packing,
        }
    }

    fn ticked_mut(&mut self) -> &mut BTreeSet<usize> {
        match self.mode {
            Mode::Open => &mut self.opened,
            Mode::Pack => &mut self.packing,
        }
    }

    /// Whether one row may be ticked. Asked on every click, so it looks the row
    /// up rather than building the whole set to ask about one of them.
    fn may_tick(&self, index: usize) -> bool {
        match self.mode {
            Mode::Open => self
                .listing
                .as_ref()
                .and_then(|listing| listing.entries.get(index))
                .is_some_and(|entry| entry.safe),
            Mode::Pack => self
                .plan
                .as_ref()
                .is_some_and(|plan| index < plan.items.len()),
        }
    }

    /// Every row that may be ticked, for the button that ticks all of them.
    fn tickable(&self) -> BTreeSet<usize> {
        match self.mode {
            Mode::Open => self
                .listing
                .as_ref()
                .map(|listing| {
                    listing
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.safe)
                        .map(|(index, _)| index)
                        .collect()
                })
                .unwrap_or_default(),
            Mode::Pack => self
                .plan
                .as_ref()
                .map(|plan| (0..plan.items.len()).collect())
                .unwrap_or_default(),
        }
    }

    /// The names the worker acts on. Names rather than indices: a 7z is read in
    /// the order its blocks were packed, not the order it is shown in.
    fn wanted(&self) -> HashSet<String> {
        match self.mode {
            Mode::Open => self
                .listing
                .as_ref()
                .map(|listing| {
                    self.opened
                        .iter()
                        .filter_map(|index| listing.entries.get(*index))
                        .map(|entry| entry.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            Mode::Pack => self
                .plan
                .as_ref()
                .map(|plan| {
                    self.packing
                        .iter()
                        .filter_map(|index| plan.items.get(*index))
                        .map(|item| item.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// What the side that is showing is looking at.
    fn source(&self) -> Option<PathBuf> {
        match self.mode {
            Mode::Open => self
                .listing
                .as_ref()
                .map(|listing| listing.source.clone())
                .or_else(|| self.pending.clone()),
            Mode::Pack => self
                .plan
                .as_ref()
                .map(|plan| plan.root.clone())
                .or_else(|| self.pending.clone()),
        }
    }
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
    /// Raised to stop a run between entries.
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
        let view = ui.global::<ArchivesState>();

        let (bound, controller) = (shell.clone(), self.clone());
        view.on_choose(move || {
            let job = match controller.state.borrow().mode {
                Mode::Open => Job::Choose(bound.language()),
                Mode::Pack => Job::ChooseFolder(bound.language()),
            };
            bound.submit(Command::Archives(job), Message::Picking);
        });
        // Switching sides shows what that side already had. Neither is read
        // again: an archive's contents do not change because the page looked
        // away from them.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_mode_selected(move |mode| {
            controller.state.borrow_mut().mode = if mode == 1 { Mode::Pack } else { Mode::Open };
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_toggle(move |index| {
            if controller.busy() || index < 0 {
                return;
            }
            {
                let mut state = controller.state.borrow_mut();
                let index = index as usize;
                // A row the engine refused is not one a page may tick: what may
                // be written is a property of the name, not of the checkbox.
                if !state.may_tick(index) {
                    return;
                }
                if !state.ticked_mut().remove(&index) {
                    state.ticked_mut().insert(index);
                }
            }
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_tick_all(move || {
            if controller.busy() {
                return;
            }
            {
                let mut state = controller.state.borrow_mut();
                let all = state.tickable();
                // The same button clears when everything is already ticked:
                // there is only one thing left to want at that point.
                let cleared = *state.ticked() == all;
                *state.ticked_mut() = if cleared { BTreeSet::new() } else { all };
            }
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_format_selected(move || {
            controller.adopt_format();
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_run(move || {
            if controller.busy() {
                return;
            }
            controller.adopt_format();
            let job = {
                let state = controller.state.borrow();
                let wanted = state.wanted();
                if wanted.is_empty() {
                    None
                } else {
                    match state.mode {
                        Mode::Open => state.listing.as_ref().map(|listing| {
                            (
                                Job::Extract {
                                    language: bound.language(),
                                    source: listing.source.clone(),
                                    wanted,
                                    cancel: controller.cancel.clone(),
                                },
                                Message::Unpacking,
                            )
                        }),
                        Mode::Pack => state.plan.as_ref().map(|plan| {
                            (
                                Job::Create {
                                    language: bound.language(),
                                    plan: plan.clone(),
                                    wanted,
                                    kind: state.kind,
                                    cancel: controller.cancel.clone(),
                                },
                                Message::Packing,
                            )
                        }),
                    }
                }
            };
            if let Some((job, message)) = job {
                controller.cancel.store(false, Ordering::Relaxed);
                // Deliberately not marked as running yet: the job opens a
                // dialog first, and a cancelled dialog leaves nothing running.
                // The first progress report is what starts the bar.
                bound.submit(Command::Archives(job), message);
            }
        });
        let controller = self.clone();
        view.on_stop(move || controller.cancel.store(true, Ordering::Relaxed));
    }

    /// Adopt a path the user already chose, from a drop or the command line. A
    /// file is an archive to look inside; a folder is something to pack, which
    /// is the only thing a folder can mean here.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("archives");
        if path.is_dir() {
            self.state.borrow_mut().mode = Mode::Pack;
            self.adopt(shell, path, Job::Survey);
        } else {
            self.state.borrow_mut().mode = Mode::Open;
            self.adopt(shell, path, Job::Read);
        }
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        match event {
            // The picker's operation ends here; reading takes the busy state on
            // its own.
            Event::Chosen(path) => {
                shell.finish(Ok(Message::Ready));
                self.state.borrow_mut().mode = Mode::Open;
                self.adopt(shell, path, Job::Read);
            }
            Event::Picked(path) => {
                shell.finish(Ok(Message::Ready));
                self.state.borrow_mut().mode = Mode::Pack;
                self.adopt(shell, path, Job::Survey);
            }
            Event::Listed(Ok(listing)) => {
                self.working(false);
                {
                    let mut state = self.state.borrow_mut();
                    state.pending = None;
                    // Everything that may be written, ticked: unpacking adds
                    // files rather than replacing them, so the whole archive is
                    // what someone almost always means.
                    state.opened = listing
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.safe)
                        .map(|(index, _)| index)
                        .collect();
                    state.listing = Some(listing);
                }
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Listed(Err(failure)) => {
                self.working(false);
                {
                    let mut state = self.state.borrow_mut();
                    state.pending = None;
                    state.listing = None;
                    state.opened.clear();
                }
                self.refresh(shell.language());
                shell.finish(Err(failure));
            }
            Event::Surveyed(Ok(plan)) => {
                self.working(false);
                {
                    let mut state = self.state.borrow_mut();
                    state.pending = None;
                    state.packing = (0..plan.items.len()).collect();
                    state.plan = Some(plan);
                }
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Surveyed(Err(failure)) => {
                self.working(false);
                {
                    let mut state = self.state.borrow_mut();
                    state.pending = None;
                    state.plan = None;
                    state.packing.clear();
                }
                self.refresh(shell.language());
                shell.finish(Err(failure));
            }
            Event::Progress { done, total } => {
                self.state.borrow_mut().progress = Some((done, total));
                self.running(true);
                self.refresh(shell.language());
            }
            // What was written is reported on the notice; the lists are what
            // they were, because neither the archive nor the folder changed.
            Event::Done => {
                self.running(false);
                self.state.borrow_mut().progress = None;
                self.refresh(shell.language());
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ArchivesState>();
        let state = self.state.borrow();
        view.set_mode(match state.mode {
            Mode::Open => 0,
            Mode::Pack => 1,
        });
        view.set_format(match state.kind {
            Kind::Zip => 0,
            Kind::SevenZ => 1,
        });
        view.set_source(match state.source() {
            Some(path) => path.display().to_string().into(),
            None => SharedString::new(),
        });
        view.set_detail(SharedString::from(match (state.mode, state.source()) {
            (Mode::Open, Some(_)) => lang.text(
                "Reads only. Nothing is unpacked until you extract, and nothing is ever replaced.",
                "只读取，不改动。只有点击解压才会写文件，而且从不覆盖已有文件。",
            ),
            (Mode::Open, None) => lang.text(
                "Choose a ZIP or 7z archive, or drop one on the window",
                "选择 ZIP 或 7z 压缩包，或把它拖到窗口上",
            ),
            (Mode::Pack, Some(_)) => lang.text(
                "Reads only. The folder is untouched; the archive is written where you choose it.",
                "只读取，不改动。原文件夹保持不变，压缩包写到你选择的位置。",
            ),
            (Mode::Pack, None) => lang.text(
                "Choose a folder to pack, or drop one on the window",
                "选择要压缩的文件夹，或把文件夹拖到窗口上",
            ),
        }));

        let rows = match state.mode {
            Mode::Open => state
                .listing
                .as_ref()
                .map(|listing| entry_rows(listing, &state.opened, lang)),
            Mode::Pack => state
                .plan
                .as_ref()
                .map(|plan| item_rows(plan, &state.packing)),
        };
        let count = rows.as_ref().map(Vec::len).unwrap_or_default();
        view.set_rows(match rows {
            Some(rows) => ModelRc::new(VecModel::from(rows)),
            None => ModelRc::default(),
        });
        view.set_has_rows(count > 0);
        view.set_summary(SharedString::from(match state.mode {
            Mode::Open => state
                .listing
                .as_ref()
                .map(|listing| listing_summary(listing, lang))
                .unwrap_or_default(),
            Mode::Pack => state
                .plan
                .as_ref()
                .map(|plan| plan_summary(plan, lang))
                .unwrap_or_default(),
        }));
        view.set_has_selection(!state.ticked().is_empty());
        view.set_selection(selection(&state, lang).into());
        let (line, tone) = status(&state, lang);
        view.set_status(line.into());
        view.set_status_tone(tone);
    }

    /// Take a path as the input and read it. Reading is an operation like any
    /// other: it takes the busy state and reports through the worker.
    fn adopt(&self, shell: &Shell, path: PathBuf, job: fn(PathBuf) -> Job) {
        {
            let mut state = self.state.borrow_mut();
            state.pending = Some(path.clone());
            match state.mode {
                Mode::Open => {
                    state.listing = None;
                    state.opened.clear();
                }
                Mode::Pack => {
                    state.plan = None;
                    state.packing.clear();
                }
            }
        }
        let message = match self.state.borrow().mode {
            Mode::Open => Message::Inspecting,
            Mode::Pack => Message::Searching,
        };
        self.refresh(shell.language());
        let accepted = shell.submit(Command::Archives(job(path)), message);
        self.working(accepted);
    }

    /// Read the format control back into the state. It is the page's own until
    /// a run starts, and this is where it becomes what gets written.
    fn adopt_format(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        self.state.borrow_mut().kind = match ui.global::<ArchivesState>().get_format() {
            1 => Kind::SevenZ,
            _ => Kind::Zip,
        };
    }

    fn busy(&self) -> bool {
        self.ui.upgrade().is_none_or(|ui| ui.get_busy())
    }

    fn working(&self, working: bool) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ArchivesState>().set_working(working);
        }
    }

    /// Whether files are being written. Separate from `working`, because the
    /// button that stops a run must not appear while a listing is being read.
    fn running(&self, running: bool) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ArchivesState>().set_running(running);
        }
    }
}

/// An archive as rows. What a row says about itself is what the archive claims:
/// its size, how much of that it takes up, and when it says it was written.
fn entry_rows(listing: &Listing, ticked: &BTreeSet<usize>, lang: Language) -> Vec<ArchiveRow> {
    listing
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut parts = Vec::new();
            if !entry.directory {
                parts.push(size(entry.size));
            }
            if let Some(modified) = &entry.modified {
                parts.push(modified.clone());
            }
            if entry.encrypted {
                parts.push(lang.text("password", "有密码").to_owned());
            }
            ArchiveRow {
                name: entry.name.as_str().into(),
                detail: if entry.safe {
                    parts.join("  ·  ").into()
                } else {
                    // The one row whose note is not a fact about the file but a
                    // statement about what this tool will do with it.
                    SharedString::from(lang.text("outside the folder", "路径越界"))
                },
                ticked: ticked.contains(&index),
                refused: !entry.safe,
                directory: entry.directory,
                index: index as i32,
            }
        })
        .collect()
}

/// A folder as rows: what would go into the archive, under the name it would
/// carry there.
fn item_rows(plan: &Plan, ticked: &BTreeSet<usize>) -> Vec<ArchiveRow> {
    plan.items
        .iter()
        .enumerate()
        .map(|(index, item)| ArchiveRow {
            name: item.name.as_str().into(),
            detail: if item.directory {
                SharedString::new()
            } else {
                size(item.size).into()
            },
            ticked: ticked.contains(&index),
            refused: false,
            directory: item.directory,
            index: index as i32,
        })
        .collect()
}

fn listing_summary(listing: &Listing, lang: Language) -> String {
    let mut parts = vec![format!(
        "{} {} {}",
        listing.kind.label(),
        listing.files,
        files(listing.files, lang)
    )];
    if listing.folders > 0 {
        parts.push(format!(
            "{} {}",
            listing.folders,
            lang.text("folders", "个目录")
        ));
    }
    parts.push(format!("{} → {}", size(listing.size), size(listing.packed)));
    if listing.size > 0 && listing.packed < listing.size {
        parts.push(format!(
            "{} {:.0}%",
            lang.text("smaller by", "压缩率"),
            listing.ratio() * 100.0
        ));
    }
    if listing.left_out > 0 {
        parts.push(format!(
            "{} {} {}",
            lang.text("and", "另有"),
            listing.left_out,
            lang.text("more that this list does not hold", "条未列出")
        ));
    }
    parts.join("  ·  ")
}

fn plan_summary(plan: &Plan, lang: Language) -> String {
    let mut line = format!(
        "{} {}  ·  {}",
        plan.items.iter().filter(|item| !item.directory).count(),
        files(
            plan.items.iter().filter(|item| !item.directory).count(),
            lang
        ),
        size(plan.size)
    );
    if plan.left_out > 0 {
        line.push_str(&format!(
            "  ·  {} {} {}",
            lang.text("and", "另有"),
            plan.left_out,
            lang.text("more that this list does not hold", "个未列出")
        ));
    }
    line
}

/// What is ticked, or where a run has got to.
fn selection(state: &State, lang: Language) -> String {
    if let Some((done, total)) = state.progress {
        return format!(
            "{} {done} / {total}…",
            match state.mode {
                Mode::Open => lang.text("Extracting", "正在解压"),
                Mode::Pack => lang.text("Packing", "正在压缩"),
            }
        );
    }
    let ticked = state.ticked();
    if ticked.is_empty() {
        return match state.mode {
            Mode::Open => lang.text(
                "Tick what to take out. Nothing is ticked yet.",
                "勾选要解压的内容；目前还没有勾选任何一条。",
            ),
            Mode::Pack => lang.text(
                "Tick what to pack. Nothing is ticked yet.",
                "勾选要打包的内容；目前还没有勾选任何一个。",
            ),
        }
        .to_owned();
    }
    let bytes: u64 = match state.mode {
        Mode::Open => state
            .listing
            .as_ref()
            .map(|listing| {
                ticked
                    .iter()
                    .filter_map(|index| listing.entries.get(*index))
                    .map(|entry| entry.size)
                    .sum()
            })
            .unwrap_or_default(),
        Mode::Pack => state
            .plan
            .as_ref()
            .map(|plan| {
                ticked
                    .iter()
                    .filter_map(|index| plan.items.get(*index))
                    .map(|item| item.size)
                    .sum()
            })
            .unwrap_or_default(),
    };
    format!(
        "{} {} {}  ·  {}",
        lang.text("Ticked", "已勾选"),
        ticked.len(),
        lang.text("entries", "条"),
        size(bytes)
    )
}

/// The sentence under the page: what to do next, or what to know before doing
/// it. Tone 2 is kept for the two things worth reading twice — a name that
/// points out of the folder, and an archive nobody here can open.
fn status(state: &State, lang: Language) -> (String, i32) {
    match state.mode {
        Mode::Open => {
            let Some(listing) = &state.listing else {
                return (
                    lang.text(
                        "Everything happens on your device. An archive is only read until you extract something out of it.",
                        "所有处理都在本机完成。在你点击解压之前，压缩包只会被读取。",
                    )
                    .to_owned(),
                    0,
                );
            };
            if listing.refused > 0 {
                return (
                    format!(
                        "{} {} {}",
                        lang.text("Marked in red:", "红色标出的"),
                        listing.refused,
                        lang.text(
                            "entries name a place outside the folder they would be unpacked into. They cannot be ticked, and are never written.",
                            "条目指向了解压目标文件夹之外的位置。它们无法勾选，也不会被写入。",
                        )
                    ),
                    2,
                );
            }
            if listing.encrypted {
                return (
                    lang.text(
                        "Parts of this archive are password protected. Those cannot be opened here.",
                        "该压缩包中有受密码保护的内容，当前版本无法打开这些部分。",
                    )
                    .to_owned(),
                    2,
                );
            }
            if listing.entries.is_empty() {
                return (
                    lang.text(
                        "This archive holds nothing at all.",
                        "这个压缩包里没有任何内容。",
                    )
                    .to_owned(),
                    0,
                );
            }
            (
                lang.text(
                    "Nothing has been written. What you tick is unpacked into the folder you choose, and a file that is already there is left alone.",
                    "目前还没有写入任何文件。勾选的内容会解压到你选择的文件夹；已存在的同名文件会被跳过。",
                )
                .to_owned(),
                1,
            )
        }
        Mode::Pack => {
            let Some(plan) = &state.plan else {
                return (
                    lang.text(
                        "Everything happens on your device. A folder is only read; the archive is written where you choose it.",
                        "所有处理都在本机完成。文件夹只会被读取，压缩包写到你选择的位置。",
                    )
                    .to_owned(),
                    0,
                );
            };
            if plan.items.is_empty() {
                return (
                    lang.text("This folder holds no files.", "这个文件夹里没有文件。")
                        .to_owned(),
                    0,
                );
            }
            (
                format!(
                    "{} {}",
                    lang.text(
                        "The folder itself is untouched. Links are never followed, so what is packed is what is actually inside it.",
                        "原文件夹不会被改动。符号链接不会被跟随，打包的就是文件夹里本来的内容。",
                    ),
                    lang.text(
                        "ZIP opens anywhere; 7z is usually smaller.",
                        "ZIP 通用性最好，7z 通常更小。",
                    )
                ),
                1,
            )
        }
    }
}

fn files(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("file", "个文件")
    } else {
        lang.text("files", "个文件")
    }
}
