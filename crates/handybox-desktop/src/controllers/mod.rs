//! UI wiring. The shell owns routing, language and the single notice surface;
//! every tool owns its own state, callbacks and events in its own module.
pub mod archives;
pub mod cleanup;
pub mod clipboard;
pub mod codes;
pub mod crypto;
pub mod diff;
pub mod documents;
pub mod images;
pub mod json;

use crate::{
    AppWindow, I18n, Theme, ToolItem, link,
    locale::{Failure, FailureKind, Language, Message, matches_tool, tool_text},
    settings,
    worker::{Command, Event, Worker},
};
use handybox_core::{
    catalog::{TOOLS, ToolDescriptor},
    tools::{
        archives as core_archives, codes as core_codes, crypto as core_crypto,
        images as core_images, json as core_json,
    },
};
use i_slint_backend_winit::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc::SyncSender, time::Duration};

#[derive(Default)]
struct State {
    language: Language,
    query: String,
    message: Message,
    preference_error: Option<String>,
}

/// Shared services a tool controller may use: the command channel, the current
/// language and the notice surface. Cheap to clone into a Slint callback.
#[derive(Clone)]
pub struct Shell {
    ui: slint::Weak<AppWindow>,
    commands: SyncSender<Command>,
    state: Rc<RefCell<State>>,
}

impl Shell {
    pub fn language(&self) -> Language {
        self.state.borrow().language
    }

    /// Hand a command to the worker and take the busy state. One operation runs
    /// at a time: navigation stays available, actions do not queue up. Returns
    /// whether the worker accepted the command.
    pub fn submit(&self, command: Command, message: Message) -> bool {
        let Some(ui) = self.ui.upgrade() else {
            return false;
        };
        if ui.get_busy() {
            return false;
        }
        let accepted = self.commands.try_send(command);
        let sent = accepted.is_ok();
        match accepted {
            Ok(()) => {
                ui.set_busy(true);
                self.state.borrow_mut().message = message;
            }
            Err(error) => {
                self.state.borrow_mut().message = Message::Error(Failure {
                    kind: FailureKind::Worker,
                    detail: error.to_string(),
                })
            }
        }
        self.refresh_notice();
        sent
    }

    /// Submit background work: no busy state, no notice, and no complaint when
    /// the worker is occupied — the caller is expected to try again.
    pub fn dispatch(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }

    /// End an operation started with [`Shell::submit`].
    pub fn finish(&self, outcome: std::result::Result<Message, Failure>) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_busy(false);
        self.state.borrow_mut().message = match outcome {
            Ok(message) => message,
            Err(failure) => Message::Error(failure),
        };
        self.refresh_notice();
    }

    /// Replace the notice without touching the busy state.
    pub fn notify(&self, message: Message) {
        self.state.borrow_mut().message = message;
        self.refresh_notice();
    }

    /// Whether a tool's page is the one on screen.
    pub fn showing(&self, key: &str) -> bool {
        self.ui
            .upgrade()
            .is_some_and(|ui| ui.get_current_tool().key == key)
    }

    pub fn navigate(&self, key: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        let language = self.language();
        if let Some(tool) = TOOLS.iter().find(|tool| tool.key == key) {
            ui.set_current_tool(item(tool, language));
        }
    }

    fn refresh_notice(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow();
        let lang = state.language;
        let is_error = state.message.is_error_notice();
        // One notice surface, owned by the shell. The status bar is shared by
        // every tool, so anything shown there would have to be cleared on
        // navigation.
        ui.set_notice_error(is_error || state.preference_error.is_some());
        ui.set_notice_long(
            state.message.is_long_notice() && (state.preference_error.is_none() || is_error),
        );
        ui.set_notice(if state.preference_error.is_some() && !is_error {
            lang.text(
                "Language changed for this session, but the preference could not be saved.",
                "语言已切换，但无法保存偏好设置；本次会话仍然有效。",
            )
            .into()
        } else if state.message.is_notice() {
            state.message.render(lang).into()
        } else {
            "".into()
        });
    }

    fn refresh_tools(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow();
        ui.set_tools(ModelRc::new(VecModel::from(
            TOOLS
                .iter()
                .filter(|tool| matches_tool(tool, &state.query))
                .map(|tool| item(tool, state.language))
                .collect::<Vec<_>>(),
        )));
        let current = ui.get_current_tool().key;
        drop(state);
        self.navigate(&current);
    }
}

fn item(tool: &ToolDescriptor, language: Language) -> ToolItem {
    let (name, subtitle, capabilities) = tool_text(tool, language);
    ToolItem {
        key: tool.key.into(),
        name: name.into(),
        subtitle: subtitle.into(),
        libraries: tool.libraries.into(),
        capabilities: capabilities.into(),
        available: tool.available,
    }
}

// A font covering the active script keeps text layout off the renderer's
// system-fallback path, which is re-queried per text item and per frame.
fn apply_language(ui: &AppWindow, language: Language) {
    ui.global::<I18n>().set_chinese(language.chinese());
    ui.global::<Theme>()
        .set_font_family(language.font_family().into());
}

/// Every tool's controller. They are needed together — to route a file, to
/// relabel after a language change, and to hand each worker event to its own
/// tool — so they travel together, and adding a tool adds a field rather than a
/// parameter to three signatures.
#[derive(Clone)]
struct Tools {
    documents: documents::Controller,
    json: json::Controller,
    crypto: crypto::Controller,
    diff: diff::Controller,
    codes: codes::Controller,
    clipboard: clipboard::Controller,
    cleanup: cleanup::Controller,
    images: images::Controller,
    archives: archives::Controller,
}

impl Tools {
    fn new(ui: &AppWindow) -> Self {
        Self {
            documents: documents::Controller::new(ui),
            json: json::Controller::new(ui),
            crypto: crypto::Controller::new(ui),
            diff: diff::Controller::new(ui),
            codes: codes::Controller::new(ui),
            clipboard: clipboard::Controller::new(ui),
            cleanup: cleanup::Controller::new(ui),
            images: images::Controller::new(ui),
            archives: archives::Controller::new(ui),
        }
    }

    fn bind(&self, shell: &Shell) {
        self.documents.bind(shell);
        self.json.bind(shell);
        self.crypto.bind(shell);
        self.diff.bind(shell);
        self.codes.bind(shell);
        self.clipboard.bind(shell);
        self.cleanup.bind(shell);
        self.images.bind(shell);
        self.archives.bind(shell);
    }

    /// Re-render what every tool derives from its state. Only presentation
    /// metadata: documents, results, selection, route and jobs in flight are
    /// untouched when the language changes.
    fn refresh(&self, language: Language) {
        self.documents.refresh(language);
        self.json.refresh(language);
        self.crypto.refresh(language);
        self.diff.refresh(language);
        self.codes.refresh(language);
        self.clipboard.refresh(language);
        self.cleanup.refresh(language);
        self.images.refresh(language);
        self.archives.refresh(language);
    }

    fn handle(&self, shell: &Shell, event: Event) {
        match event {
            Event::Documents(event) => self.documents.handle(shell, event),
            Event::Json(event) => self.json.handle(shell, event),
            Event::Crypto(event) => self.crypto.handle(shell, event),
            Event::Diff(event) => self.diff.handle(shell, event),
            Event::Codes(event) => self.codes.handle(shell, event),
            Event::Clipboard(event) => self.clipboard.handle(shell, event),
            Event::Cleanup(event) => self.cleanup.handle(shell, event),
            Event::Images(event) => self.images.handle(shell, event),
            Event::Archives(event) => self.archives.handle(shell, event),
            Event::Finished(outcome) => shell.finish(outcome),
        }
    }

    /// Editing is not an operation: the workbench validates and the diff tool
    /// compares on their own clock, and the clipboard workspace re-states how
    /// long ago it collected what it is showing.
    fn tick(&self, shell: &Shell) {
        self.json.tick(shell);
        self.diff.tick(shell);
        self.clipboard.tick(shell);
    }

    /// Hand a file to the tool that handles it. The extension is all there is to
    /// go on before reading it, and it is enough to tell these apart — except
    /// for the two tools that take any file at all: the crypto tool, and the
    /// diff tool, whose input is whatever text you put beside another text.
    /// While one of their pages is showing, a dropped file is its input rather
    /// than something to route away.
    fn open(&self, shell: &Shell, path: PathBuf) {
        // A folder means something to three tools now, so the page that is
        // showing decides: the image studio lists the pictures in it, Archives
        // packs it, and Disk cleanup — which is what a folder means everywhere
        // else — reads it.
        if path.is_dir() {
            if shell.showing("images") {
                self.images.open(shell, path);
            } else if shell.showing("archives") {
                self.archives.open(shell, path);
            } else {
                self.cleanup.open(shell, path);
            }
        } else if shell.showing("crypto") {
            self.crypto.open(shell, path, None);
        // Two tools take a picture, so the page that is showing decides which
        // one a dropped image belongs to; the barcode reader keeps the claim
        // when neither is on screen, because reading a code off a picture is
        // the answer to a question, and opening it is not.
        } else if shell.showing("images") && core_images::claims(&path) {
            self.images.open(shell, path);
        } else if shell.showing("diff") {
            self.diff.open(shell, path);
        } else if core_crypto::claims(&path) {
            self.crypto.open(shell, path, Some(crypto::Mode::Decrypt));
        } else if core_archives::claims(&path) {
            self.archives.open(shell, path);
        } else if core_codes::claims(&path) {
            self.codes.open(shell, path);
        } else if core_json::claims(&path) {
            self.json.open(shell, path);
        } else {
            self.documents.convert(shell, path);
        }
    }
}

/// Byte counts are shown to people, not parsed: one decimal, one unit.
pub fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// Retain the returned timer for as long as the window is alive. `initial` is
/// what the command line named: one file opens in the tool that handles it, two
/// are a comparison.
pub fn bind(ui: &AppWindow, initial: Vec<PathBuf>) -> anyhow::Result<Timer> {
    let worker = Worker::start()?;
    let shell = Shell {
        ui: ui.as_weak(),
        commands: worker.commands.clone(),
        state: Rc::new(RefCell::new(State {
            language: settings::load_language(),
            ..Default::default()
        })),
    };
    let tools = Tools::new(ui);

    ui.set_repository(link::REPOSITORY.into());
    apply_language(ui, shell.language());
    shell.navigate("documents");
    shell.refresh_tools();
    shell.refresh_notice();
    tools.bind(&shell);
    tools.refresh(shell.language());

    let bound = shell.clone();
    ui.on_navigate(move |key| bound.navigate(&key));
    let bound = shell.clone();
    ui.on_search_tools(move |query| {
        bound.state.borrow_mut().query = query.to_string();
        bound.refresh_tools();
    });
    let (bound, relabel) = (shell.clone(), tools.clone());
    ui.on_language_selected(move |chinese| {
        let Some(ui) = bound.ui.upgrade() else { return };
        {
            let mut state = bound.state.borrow_mut();
            state.language = Language::from_chinese(chinese);
            // Only a tiny atomic preferences write, never document I/O.
            state.preference_error = settings::save_language(state.language)
                .err()
                .map(|e| e.to_string());
        }
        apply_language(&ui, bound.language());
        bound.refresh_tools();
        bound.refresh_notice();
        relabel.refresh(bound.language());
    });
    let bound = shell.clone();
    // The toast expires on its own; clearing the message keeps a later refresh
    // from bringing the same notice back.
    ui.on_dismiss_notice(move || {
        {
            let mut state = bound.state.borrow_mut();
            state.message = Message::Ready;
            state.preference_error = None;
        }
        bound.refresh_notice();
    });
    let bound = shell.clone();
    ui.on_open_repository(move || {
        // Spawning the browser is immediate; it never occupies the worker.
        if let Err(error) = link::open(&format!("https://{}", link::REPOSITORY)) {
            bound.notify(Message::Error(Failure {
                kind: FailureKind::Operation,
                detail: error.to_string(),
            }));
        }
    });

    // A file dropped anywhere in the window opens in the tool that handles it,
    // whichever page is showing, so the route follows the file.
    let (bound, dropped, weak) = (shell.clone(), tools.clone(), ui.as_weak());
    ui.window().on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        match event {
            WindowEvent::HoveredFile(_) if !ui.get_busy() => ui.set_dragging(true),
            WindowEvent::HoveredFileCancelled => ui.set_dragging(false),
            WindowEvent::DroppedFile(path) => {
                ui.set_dragging(false);
                if !ui.get_busy() {
                    dropped.open(&bound, path.clone());
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });

    match initial.as_slice() {
        [file] => tools.open(&shell, file.clone()),
        [left, right] => tools.diff.open_pair(&shell, left.clone(), right.clone()),
        _ => {}
    }
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        if weak.upgrade().is_none() {
            return;
        };
        for event in worker.events.try_iter() {
            tools.handle(&shell, event);
        }
        tools.tick(&shell);
    });
    Ok(timer)
}
