//! LAN share: one folder, open to the network for as long as someone wants it.
//!
//! The state this tool keeps is unusual for this box in one way: the server is
//! not here. It lives in the worker, because it has to outlive the command that
//! started it, and what this controller holds is only a *picture* of it —
//! whether it is running, on which address, and what was in the folder the last
//! time anyone looked. `sharing` is set from the worker's own events and never
//! optimistically from a click, so the page cannot show an address for a server
//! that failed to start.
//!
//! The listing refreshes on a timer, through `dispatch` rather than `submit`:
//! a phone uploading a file is not this window's operation, and finding out
//! about it must not take the busy state or interrupt anything.
use super::Shell;
use crate::{
    AppWindow, ShareRow, ShareState,
    locale::{Failure, Language, Message, size},
    settings,
    worker::{Command, share::Command as Job, share::Event},
};
use handybox_core::tools::share::{self, Entry};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

/// How often the shared folder is re-read while sharing. The served page polls
/// at the same rate, so both sides notice a new file at about the same moment.
const REFRESH: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    sharing: bool,
    /// Where the address came from. Retained rather than rendered, so a
    /// language change re-states the sentence without restarting anything.
    address: Option<Address>,
    /// The folder that will be shared next, or is being shared now. Set from
    /// the saved preference at startup, so the page can name it before anything
    /// has been started.
    root: Option<PathBuf>,
    files: Vec<Entry>,
    /// When the listing was last asked for, so the timer can pace itself
    /// without a second clock.
    listed: Option<Instant>,
    /// The name of the text file open in the reading sheet.
    previewing: Option<String>,
    preview: String,
    rendered: Option<Instant>,
    list_error: Option<Failure>,
    sent_language: Option<bool>,
}

struct Address {
    url: String,
    others: Vec<String>,
    reachable: bool,
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
    rows: Rc<VecModel<ShareRow>>,
}

impl Controller {
    pub fn new(ui: &AppWindow) -> Self {
        Self {
            ui: ui.as_weak(),
            rows: Rc::new(VecModel::default()),
            state: Rc::new(RefCell::new(State {
                root: settings::load_share_root().or_else(share::default_root),
                ..Default::default()
            })),
        }
    }

    pub fn bind(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ShareState>();
        view.set_files(ModelRc::from(self.rows.clone()));

        let (bound, controller) = (shell.clone(), self.clone());
        view.on_start(move || {
            let Some(root) = controller.state.borrow().root.clone() else {
                return bound.notify(Message::Error(Failure::not_sharing()));
            };
            let chinese = bound.language().chinese();
            if bound.submit(
                Command::Share(Job::Start { root, chinese }),
                Message::Starting,
            ) && let Some(ui) = controller.ui.upgrade()
            {
                ui.global::<ShareState>().set_starting(true);
            }
        });
        let bound = shell.clone();
        view.on_stop(move || {
            bound.submit(Command::Share(Job::Stop), Message::Stopped);
        });
        let bound = shell.clone();
        view.on_choose_folder(move || {
            bound.submit(
                Command::Share(Job::ChooseRoot(bound.language())),
                Message::Picking,
            );
        });
        let bound = shell.clone();
        view.on_choose_files(move || {
            bound.submit(
                Command::Share(Job::Choose(bound.language())),
                Message::Picking,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_save_text(move || {
            let Some(ui) = controller.ui.upgrade() else {
                return;
            };
            let text = ui.global::<ShareState>().get_draft().to_string();
            if text.trim().is_empty() {
                return;
            }
            bound.submit(Command::Share(Job::SaveText(text)), Message::Saving);
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy_address(move || {
            if let Some(address) = &controller.state.borrow().address {
                bound.submit(Command::Copy(address.url.clone()), Message::Copying);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_open_browser(move || {
            let url = controller
                .state
                .borrow()
                .address
                .as_ref()
                .map(|address| address.url.clone());
            if let Some(url) = url
                && let Err(error) = crate::link::open(&url)
            {
                bound.notify(Message::Error(Failure::operation(error)));
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_open_folder(move || {
            let root = controller.state.borrow().root.clone();
            if let Some(root) = root
                && let Err(error) = crate::link::open(&root.to_string_lossy())
            {
                bound.notify(Message::Error(Failure::operation(error)));
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_view(move |index| {
            if let Some(name) = controller.name(index) {
                bound.submit(Command::Share(Job::ReadText(name)), Message::Reading);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_reveal(move |index| {
            if let Some(name) = controller.name(index) {
                bound.submit(Command::Share(Job::Reveal(name)), Message::Opening);
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_remove(move |index| {
            if let Some(name) = controller.name(index) {
                bound.submit(Command::Share(Job::Delete(name)), Message::Removing);
            }
        });
        let controller = self.clone();
        view.on_close_preview(move || {
            let mut state = controller.state.borrow_mut();
            state.previewing = None;
            state.preview.clear();
            if let Some(ui) = controller.ui.upgrade() {
                ui.global::<ShareState>().set_previewing(false);
                ui.global::<ShareState>()
                    .set_preview_lines(ModelRc::default());
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy_preview(move || {
            let text = controller.state.borrow().preview.clone();
            if !text.is_empty() {
                bound.submit(Command::Copy(text), Message::Copying);
            }
        });
    }

    /// Adopt a selection the user already chose: a drop on this page, or the
    /// files picker. The whole selection goes in at once, because a drop of ten
    /// files is one thing the user did.
    ///
    /// Only while sharing: putting a file into a folder nobody is serving would
    /// look like it had been sent somewhere. Said plainly instead, since the
    /// page in front is this one and nothing else is going to take the file.
    pub fn open(&self, shell: &Shell, paths: Vec<PathBuf>) {
        if !self.state.borrow().sharing {
            return shell.notify(Message::Error(Failure::not_sharing()));
        }
        // The shared folder is flat, so a folder cannot go into it as a folder.
        // Said here rather than counted as a failure below, because "compress it
        // first" is the answer and a count is not.
        let (files, folders): (Vec<_>, Vec<_>) = paths.into_iter().partition(|path| !path.is_dir());
        if files.is_empty() && !folders.is_empty() {
            return shell.notify(Message::Error(Failure::share(anyhow::anyhow!(
                share::ShareIssue::NotFile
            ))));
        }
        shell.submit(Command::Share(Job::Import(files)), Message::Saving);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ShareState>();
        match event {
            Event::Started {
                url,
                urls,
                root,
                reachable,
                qr,
                ..
            } => {
                view.set_starting(false);
                view.set_sharing(true);
                view.set_has_qr(qr.is_some());
                view.set_qr(match qr {
                    Some(qr) => {
                        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                            &qr.rgba, qr.size, qr.size,
                        ))
                    }
                    None => Image::default(),
                });
                {
                    let mut state = self.state.borrow_mut();
                    state.sharing = true;
                    state.listed = Some(Instant::now());
                    state.sent_language = None;
                    state.root = Some(root);
                    state.address = Some(Address {
                        others: urls.into_iter().filter(|other| *other != url).collect(),
                        url,
                        reachable,
                    });
                }
                self.refresh(shell.language());
            }
            Event::Stopped | Event::Lost => {
                let lost = matches!(event, Event::Lost);
                view.set_starting(false);
                view.set_sharing(false);
                view.set_has_qr(false);
                view.set_qr(Image::default());
                {
                    let mut state = self.state.borrow_mut();
                    state.sharing = false;
                    state.address = None;
                    state.files.clear();
                    state.list_error = None;
                    state.previewing = None;
                    state.preview.clear();
                    state.listed = None;
                    view.set_previewing(false);
                    view.set_preview_lines(ModelRc::default());
                }
                self.refresh(shell.language());
                // A server that stopped on its own never finished a command, so
                // there is no busy state to end — only something to say.
                if lost {
                    shell.notify(Message::Lost);
                }
            }
            Event::Listed(Err(error)) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.files.clear();
                    state.list_error = Some(error);
                }
                self.refresh(shell.language());
            }
            Event::Listed(Ok(files)) => {
                {
                    let mut state = self.state.borrow_mut();
                    // Keep the model and scroll position when polling finds no
                    // changes. Relative timestamps only need a minute refresh.
                    if state.files == files
                        && state.list_error.is_none()
                        && state
                            .rendered
                            .is_some_and(|last| last.elapsed() < Duration::from_secs(60))
                    {
                        return;
                    }
                    state.files = files;
                    state.list_error = None;
                    state.rendered = Some(Instant::now());
                }
                self.refresh(shell.language());
            }
            Event::RootChosen(root) => {
                // A folder that cannot be remembered is still the folder for
                // this session; say so rather than refusing the choice.
                let saved = settings::save_share_root(&root);
                self.state.borrow_mut().root = Some(root);
                self.refresh(shell.language());
                if let Err(error) = saved {
                    shell.notify(Message::Error(Failure::operation(error)));
                }
            }
            Event::TextSaved(text) => {
                // Do not discard edits made while the worker was saving.
                if view.get_draft() == text.as_str() {
                    view.set_draft(SharedString::new());
                }
            }
            Event::Text { name, text } => {
                view.set_preview_lines(ModelRc::new(VecModel::from(preview_lines(&text))));
                {
                    let mut state = self.state.borrow_mut();
                    state.previewing = Some(name);
                    state.preview = text;
                }
                view.set_previewing(true);
                self.refresh(shell.language());
            }
        }
    }

    /// Re-render every string this tool derives from its state. The server is
    /// untouched: a language change re-labels the page and tells the served
    /// page to follow, and nothing stops or restarts.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ShareState>();
        let state = self.state.borrow();
        view.set_folder(match &state.root {
            Some(root) => root.display().to_string().into(),
            None => SharedString::from(lang.text("No folder chosen yet", "尚未选择文件夹")),
        });
        match &state.address {
            Some(address) => {
                view.set_address(address.url.as_str().into());
                view.set_reachable(address.reachable);
                view.set_has_other_addresses(!address.others.is_empty());
                view.set_other_addresses(
                    match address.others.is_empty() {
                        true => String::new(),
                        false => format!(
                            "{} {}",
                            lang.text("Also reachable at", "也可通过以下地址访问："),
                            address.others.join("  ·  ")
                        ),
                    }
                    .into(),
                );
            }
            None => {
                view.set_address(SharedString::new());
                view.set_reachable(true);
                view.set_has_other_addresses(false);
                view.set_other_addresses(SharedString::new());
            }
        }
        view.set_status(status(&state, lang).into());
        view.set_list_error(
            state
                .list_error
                .as_ref()
                .map(|error| error.render(lang))
                .unwrap_or_default()
                .into(),
        );
        let rows: Vec<_> = state
            .files
            .iter()
            .enumerate()
            .map(|(index, entry)| present(index, entry, lang))
            .collect();
        if self.rows.row_count() != rows.len() {
            self.rows.set_vec(rows);
        } else {
            for (index, row) in rows.into_iter().enumerate() {
                if self.rows.row_data(index).as_ref() != Some(&row) {
                    self.rows.set_row_data(index, row);
                }
            }
        }
        view.set_has_files(!state.files.is_empty());
        view.set_summary(summary(&state.files, lang).into());
        view.set_can_save_text(state.sharing);
        view.set_preview_name(match &state.previewing {
            Some(name) => name.as_str().into(),
            None => SharedString::new(),
        });
    }

    /// A failed start still finishes the command; release its local spinner too.
    pub fn finished(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ShareState>().set_starting(false);
        }
    }

    /// Re-read the shared folder while sharing, and tell the served page which
    /// language to open in. Both go out through `dispatch`: neither is a user
    /// action, and neither may take the busy state or complain about a worker
    /// that is occupied — the next tick will do just as well.
    pub fn tick(&self, shell: &Shell) {
        if !self.state.borrow().sharing {
            return;
        }
        self.relanguage(shell);
        let due = self
            .state
            .borrow()
            .listed
            .is_none_or(|last| last.elapsed() >= REFRESH);
        if due
            && shell.dispatch(Command::Share(Job::List {
                visible: shell.showing("share"),
            }))
        {
            self.state.borrow_mut().listed = Some(Instant::now());
        }
    }

    /// Follow the desktop's language on the served page, from the next load.
    pub fn relanguage(&self, shell: &Shell) {
        let chinese = shell.language().chinese();
        let needs_update = {
            let state = self.state.borrow();
            state.sharing && state.sent_language != Some(chinese)
        };
        if needs_update && shell.dispatch(Command::Share(Job::Language(chinese))) {
            self.state.borrow_mut().sent_language = Some(chinese);
        }
    }

    /// The name behind a row. The listing is re-read on a timer, so a row index
    /// is only good for as long as it takes to click it — which is why the name
    /// is resolved here and the worker is given that instead.
    fn name(&self, index: i32) -> Option<String> {
        self.state
            .borrow()
            .files
            .get(usize::try_from(index).ok()?)
            .map(|entry| entry.name.clone())
    }
}

/// What the row says about one file: how big, and how long ago.
fn present(index: usize, entry: &Entry, lang: Language) -> ShareRow {
    let tag = if entry.is_text {
        "TXT".to_owned()
    } else {
        match entry.name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && extension.len() <= 5 => {
                extension.to_uppercase()
            }
            _ => lang.text("FILE", "文件").to_owned(),
        }
    };
    ShareRow {
        name: entry.name.as_str().into(),
        detail: format!("{}  ·  {}", size(entry.size), ago(entry.modified, lang)).into(),
        tag: tag.into(),
        is_text: entry.is_text,
        index: index as i32,
    }
}

/// How long ago something arrived, to the unit that matters. Times in the
/// shared folder are recent almost by definition, so this reads better than a
/// date — and a date is one more thing to translate.
fn ago(modified: u64, lang: Language) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // A file whose clock is ahead of ours is not from the future; it is a file
    // that just arrived.
    let seconds = now.saturating_sub(modified);
    match seconds {
        0..60 => lang.text("just now", "刚刚").to_owned(),
        60..3600 => format!("{} {}", seconds / 60, lang.text("min ago", "分钟前")),
        3600..86_400 => format!("{} {}", seconds / 3600, lang.text("h ago", "小时前")),
        _ => format!("{} {}", seconds / 86_400, lang.text("d ago", "天前")),
    }
}

fn summary(files: &[Entry], lang: Language) -> String {
    if files.is_empty() {
        return String::new();
    }
    let total: u64 = files.iter().map(|entry| entry.size).sum();
    format!(
        "{} {}  ·  {}",
        files.len(),
        if files.len() == 1 {
            lang.text("file", "个文件")
        } else {
            lang.text("files", "个文件")
        },
        size(total)
    )
}

/// The line under the address. Says what is true right now, and — while
/// sharing — who can reach it, because that is the part worth repeating.
fn status(state: &State, lang: Language) -> String {
    if !state.sharing {
        return lang
            .text(
                "Nothing is being served. No port is open until you start.",
                "当前未共享，开始之前不会打开任何端口。",
            )
            .to_owned();
    }
    match &state.address {
        Some(address) if !address.reachable => lang
            .text(
                "This machine is not on a network right now, so only it can reach this address.",
                "这台机器当前没有连接网络，因此只有本机能访问该地址。",
            )
            .to_owned(),
        _ => lang
            .text(
                "Anyone on this network can open the address, download what is here, and upload into this folder.",
                "本网络下的任何人都可以打开该地址、下载这里的内容，并向该文件夹上传文件。",
            )
            .to_owned(),
    }
}

/// Bound each shaped row as well as virtualizing the list. A single unbroken
/// megabyte of CJK text must not turn into one expensive Slint Text element.
fn preview_lines(text: &str) -> Vec<SharedString> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut start = 0;
        for (count, (at, _)) in line.char_indices().enumerate() {
            if count > 0 && count % 160 == 0 {
                rows.push(line[start..at].into());
                start = at;
            }
        }
        rows.push(line[start..].into());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_recovers_and_preserves_user_edits_and_list_model() {
        use slint::platform::{
            Platform, WindowAdapter,
            software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        };
        struct Headless;
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
            }
        }
        slint::platform::set_platform(Box::new(Headless)).unwrap();
        let ui = AppWindow::new().unwrap();
        let (commands, receiver) = std::sync::mpsc::sync_channel(1);
        let shell = Shell {
            ui: ui.as_weak(),
            commands,
            state: Rc::new(RefCell::new(super::super::State::default())),
        };
        let controller = Controller::new(&ui);
        controller.state.borrow_mut().root = Some(PathBuf::from("/unused-test-folder"));
        controller.bind(&shell);
        let view = ui.global::<ShareState>();
        view.invoke_start();
        assert!(view.get_starting());
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Command::Share(Job::Start { .. })
        ));
        controller.finished();
        shell.finish(Err(Failure::not_sharing()));
        assert!(!view.get_starting());
        assert!(!ui.get_busy());

        controller.handle(
            &shell,
            Event::Started {
                url: "http://127.0.0.1:8765".into(),
                urls: vec![],
                root: PathBuf::from("/unused-test-folder"),
                reachable: false,
                qr: None,
            },
        );
        assert!(!view.get_has_qr());
        let entries = vec![Entry {
            name: "note.txt".into(),
            size: 4,
            modified: 0,
            is_text: true,
        }];
        controller.handle(&shell, Event::Listed(Ok(entries.clone())));
        let model = view.get_files();
        controller.handle(&shell, Event::Listed(Ok(entries)));
        assert_eq!(view.get_files(), model);
        assert_eq!(model.row_count(), 1);

        view.set_draft("saved".into());
        controller.handle(&shell, Event::TextSaved("saved".into()));
        assert!(view.get_draft().is_empty());
        view.set_draft("new edits".into());
        controller.handle(&shell, Event::TextSaved("saved".into()));
        assert_eq!(view.get_draft(), "new edits");

        // A full worker queue must not permanently lose a language change.
        shell
            .commands
            .try_send(Command::Copy(String::new()))
            .unwrap();
        controller.relanguage(&shell);
        assert_eq!(controller.state.borrow().sent_language, None);
        receiver.try_recv().unwrap();
        controller.relanguage(&shell);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Command::Share(Job::Language(false))
        ));

        controller.handle(&shell, Event::Listed(Err(Failure::not_sharing())));
        assert!(!view.get_has_files());
        assert!(!view.get_list_error().is_empty());
        controller.handle(
            &shell,
            Event::Text {
                name: "note.txt".into(),
                text: "hello".into(),
            },
        );
        assert!(view.get_previewing());
        controller.handle(&shell, Event::Stopped);
        assert!(!view.get_previewing());
        assert_eq!(view.get_preview_lines().row_count(), 0);
        assert!(view.get_list_error().is_empty());
    }

    #[test]
    fn preview_bounds_long_unicode_rows_without_losing_text() {
        let text = "你好🦀".repeat(1000);
        let rows = preview_lines(&text);
        assert!(rows.iter().all(|row| row.chars().count() <= 160));
        assert_eq!(rows.iter().map(|s| s.as_str()).collect::<String>(), text);
        assert_eq!(preview_lines("a\n\nb\n"), vec!["a", "", "b", ""]);
    }
}
