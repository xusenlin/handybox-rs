//! UI wiring. The shell owns routing, language and the single notice surface;
//! every tool owns its own state, callbacks and events in its own module.
pub mod documents;
pub mod json;

use crate::{
    AppWindow, I18n, Theme, ToolItem, link,
    locale::{Failure, FailureKind, Language, Message, matches_tool, tool_text},
    settings,
    worker::{Command, Event, Worker},
};
use handybox_core::{
    catalog::{TOOLS, ToolDescriptor},
    tools::json as core_json,
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
        let is_error = matches!(state.message, Message::Error(_));
        // One notice surface, owned by the shell. The status bar is shared by
        // every tool, so anything shown there would have to be cleared on
        // navigation.
        ui.set_notice_error(is_error || state.preference_error.is_some());
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

/// Hand a file to the tool that handles it. The extension is all there is to go
/// on before reading it, and it is enough to tell these two apart.
fn open(shell: &Shell, documents: &documents::Controller, json: &json::Controller, path: PathBuf) {
    if core_json::claims(&path) {
        json.open(shell, path);
    } else {
        documents.convert(shell, path);
    }
}

/// Byte counts are shown to people, not parsed: one decimal, one unit.
pub fn kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

/// Retain the returned timer for as long as the window is alive.
pub fn bind(ui: &AppWindow, initial_file: Option<PathBuf>) -> anyhow::Result<Timer> {
    let worker = Worker::start()?;
    let shell = Shell {
        ui: ui.as_weak(),
        commands: worker.commands.clone(),
        state: Rc::new(RefCell::new(State {
            language: settings::load_language(),
            ..Default::default()
        })),
    };
    let documents = documents::Controller::new(ui);
    let json = json::Controller::new(ui);

    ui.set_repository(link::REPOSITORY.into());
    apply_language(ui, shell.language());
    shell.navigate("documents");
    shell.refresh_tools();
    shell.refresh_notice();
    documents.bind(&shell);
    json.bind(&shell);
    documents.refresh(shell.language());
    json.refresh(shell.language());

    let bound = shell.clone();
    ui.on_navigate(move |key| bound.navigate(&key));
    let bound = shell.clone();
    ui.on_search_tools(move |query| {
        bound.state.borrow_mut().query = query.to_string();
        bound.refresh_tools();
    });
    let bound = shell.clone();
    // Only presentation metadata is refreshed. Documents, results, selection,
    // route and jobs in flight are untouched when the language changes.
    let (relabel_documents, relabel_json) = (documents.clone(), json.clone());
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
        relabel_documents.refresh(bound.language());
        relabel_json.refresh(bound.language());
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
    let (bound, dropped_documents, dropped_json, weak) =
        (shell.clone(), documents.clone(), json.clone(), ui.as_weak());
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
                    open(&bound, &dropped_documents, &dropped_json, path.clone());
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });

    if let Some(path) = initial_file {
        open(&shell, &documents, &json, path);
    }
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        if weak.upgrade().is_none() {
            return;
        };
        for event in worker.events.try_iter() {
            match event {
                Event::Documents(event) => documents.handle(&shell, event),
                Event::Json(event) => json.handle(&shell, event),
                Event::Finished(outcome) => shell.finish(outcome),
            }
        }
        // Editing is not an operation: the workbench validates on its own clock.
        json.tick(&shell);
    });
    Ok(timer)
}
