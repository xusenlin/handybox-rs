use crate::{
    AppWindow, I18n, Theme, ToolItem, link,
    locale::{Failure, FailureKind, Language, Message, matches_tool, tool_text},
    settings,
    worker::{Command, Event, Worker},
};
use handybox_core::{
    catalog::{TOOLS, ToolDescriptor},
    tools::documents::ConvertedDocument,
};
use i_slint_backend_winit::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc::SyncSender, time::Duration};

#[derive(Default)]
struct AppState {
    result: Option<ConvertedDocument>,
    language: Language,
    query: String,
    message: Message,
    source: Option<PathBuf>,
    preference_error: Option<String>,
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

fn navigate(ui: &AppWindow, key: &str, language: Language) {
    if let Some(tool) = TOOLS.iter().find(|tool| tool.key == key) {
        ui.set_current_tool(item(tool, language));
    }
}

// A font covering the active script keeps text layout off the renderer's
// system-fallback path, which is re-queried per text item and per frame.
fn apply_language(ui: &AppWindow, language: Language) {
    ui.global::<I18n>().set_chinese(language.chinese());
    ui.global::<Theme>()
        .set_font_family(language.font_family().into());
}

fn refresh_tools(ui: &AppWindow, state: &AppState) {
    ui.set_tools(ModelRc::new(VecModel::from(
        TOOLS
            .iter()
            .filter(|tool| matches_tool(tool, &state.query))
            .map(|tool| item(tool, state.language))
            .collect::<Vec<_>>(),
    )));
    navigate(ui, &ui.get_current_tool().key, state.language);
}

// Only presentation metadata is refreshed. Markdown, selection, route and jobs
// are untouched when the language changes.
fn refresh_text(ui: &AppWindow, state: &AppState) {
    let lang = state.language;
    let is_error = matches!(state.message, Message::Error(_));
    let message = state.message.render(lang);
    // One notice surface, owned by the page. The status bar is shared by every
    // tool, so anything shown there would have to be cleared on navigation.
    ui.set_notice_error(is_error || state.preference_error.is_some());
    ui.set_notice(if state.preference_error.is_some() && !is_error {
        lang.text(
            "Language changed for this session, but the preference could not be saved.",
            "语言已切换，但无法保存偏好设置；本次会话仍然有效。",
        )
        .into()
    } else if state.message.is_notice() {
        message.into()
    } else {
        "".into()
    });
    if let Some(document) = &state.result {
        ui.set_file_detail(
            format!(
                "{} · {:.1} KiB · {} {} ms",
                document.format,
                document.input_bytes as f64 / 1024.0,
                lang.text("Converted in", "转换耗时"),
                document.elapsed.as_millis()
            )
            .into(),
        );
        ui.set_stats(
            format!(
                "{} {} · {} {} · UTF-8",
                document.markdown.chars().count(),
                lang.text("characters", "字符"),
                document.markdown.lines().count(),
                lang.text("lines", "行")
            )
            .into(),
        );
    } else if let Some(source) = &state.source {
        ui.set_file_detail(source.display().to_string().into());
    } else {
        ui.set_file_detail(
            lang.text(
                "Office, PDF, EPUB and more · Automatically detects the format",
                "支持 Office、PDF、EPUB 等文档 · 自动识别格式",
            )
            .into(),
        );
    }
}

fn submit(
    ui: &AppWindow,
    sender: &SyncSender<Command>,
    state: &mut AppState,
    command: Command,
    message: Message,
) {
    if ui.get_busy() {
        return;
    }
    match sender.try_send(command) {
        Ok(()) => {
            ui.set_busy(true);
            state.message = message;
        }
        Err(error) => {
            state.message = Message::Error(Failure {
                kind: FailureKind::Worker,
                detail: error.to_string(),
            })
        }
    }
    refresh_text(ui, state);
}

/// Retain the returned timer for as long as the window is alive.
pub fn bind(ui: &AppWindow, initial_file: Option<PathBuf>) -> anyhow::Result<Timer> {
    let worker = Worker::start()?;
    let state = Rc::new(RefCell::new(AppState {
        language: settings::load_language(),
        ..Default::default()
    }));
    ui.set_repository(link::REPOSITORY.into());
    apply_language(ui, state.borrow().language);
    navigate(ui, "documents", state.borrow().language);
    refresh_tools(ui, &state.borrow());
    refresh_text(ui, &state.borrow());

    let weak = ui.as_weak();
    let nav_state = state.clone();
    ui.on_navigate(move |key| {
        if let Some(ui) = weak.upgrade() {
            navigate(&ui, &key, nav_state.borrow().language);
        }
    });
    let weak = ui.as_weak();
    let search_state = state.clone();
    ui.on_search_tools(move |query| {
        if let Some(ui) = weak.upgrade() {
            let mut state = search_state.borrow_mut();
            state.query = query.to_string();
            refresh_tools(&ui, &state);
        }
    });
    let weak = ui.as_weak();
    let language_state = state.clone();
    ui.on_language_selected(move |chinese| {
        if let Some(ui) = weak.upgrade() {
            let mut state = language_state.borrow_mut();
            state.language = Language::from_chinese(chinese);
            // Only a tiny atomic preferences write, never document I/O.
            state.preference_error = settings::save_language(state.language)
                .err()
                .map(|e| e.to_string());
            apply_language(&ui, state.language);
            refresh_tools(&ui, &state);
            refresh_text(&ui, &state);
        }
    });
    let weak = ui.as_weak();
    let dismiss_state = state.clone();
    // The toast expires on its own; clearing the message keeps a later refresh
    // from bringing the same notice back.
    ui.on_dismiss_notice(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = dismiss_state.borrow_mut();
            state.message = Message::Ready;
            state.preference_error = None;
            refresh_text(&ui, &state);
        }
    });
    let weak = ui.as_weak();
    let repository_state = state.clone();
    ui.on_open_repository(move || {
        if let Some(ui) = weak.upgrade() {
            // Spawning the browser is immediate; it never occupies the worker.
            if let Err(error) = link::open(&format!("https://{}", link::REPOSITORY)) {
                let mut state = repository_state.borrow_mut();
                state.message = Message::Error(Failure {
                    kind: FailureKind::Operation,
                    detail: error.to_string(),
                });
                refresh_text(&ui, &state);
            }
        }
    });
    let weak = ui.as_weak();
    let sender = worker.commands.clone();
    let pick_state = state.clone();
    ui.on_choose_document(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = pick_state.borrow_mut();
            let language = state.language;
            submit(
                &ui,
                &sender,
                &mut state,
                Command::PickDocument(language),
                Message::Picking,
            );
        }
    });
    let weak = ui.as_weak();
    let sender = worker.commands.clone();
    let copy_state = state.clone();
    ui.on_copy_markdown(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = copy_state.borrow_mut();
            if let Some(result) = &state.result {
                let command = Command::Copy(result.markdown.clone());
                submit(&ui, &sender, &mut state, command, Message::Copying);
            }
        }
    });
    let weak = ui.as_weak();
    let sender = worker.commands.clone();
    let save_state = state.clone();
    ui.on_save_markdown(move || {
        if let Some(ui) = weak.upgrade() {
            let mut state = save_state.borrow_mut();
            if let Some(result) = &state.result {
                let command = Command::Export {
                    language: state.language,
                    source: result.source.clone(),
                    name: result.suggested_name(),
                    markdown: result.markdown.clone(),
                };
                submit(&ui, &sender, &mut state, command, Message::Saving);
            }
        }
    });
    let weak = ui.as_weak();
    let sender = worker.commands.clone();
    let drop_state = state.clone();
    ui.window().on_winit_window_event(move |_, event| {
        if let Some(ui) = weak.upgrade() {
            match event {
                WindowEvent::HoveredFile(_) if !ui.get_busy() => ui.set_dragging(true),
                WindowEvent::HoveredFileCancelled => ui.set_dragging(false),
                WindowEvent::DroppedFile(path) => {
                    ui.set_dragging(false);
                    if !ui.get_busy() {
                        let mut state = drop_state.borrow_mut();
                        navigate(&ui, "documents", state.language);
                        submit(
                            &ui,
                            &sender,
                            &mut state,
                            Command::Convert(path.clone()),
                            Message::Reading,
                        );
                    }
                }
                _ => {}
            }
        }
        EventResult::Propagate
    });

    if let Some(path) = initial_file {
        submit(
            ui,
            &worker.commands,
            &mut state.borrow_mut(),
            Command::Convert(path),
            Message::Reading,
        );
    }
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let Some(ui) = weak.upgrade() else { return };
        for event in worker.events.try_iter() {
            let mut state = state.borrow_mut();
            match event {
                Event::Started(path) => {
                    state.result = None;
                    ui.set_converting(true);
                    ui.set_has_result(false);
                    ui.set_markdown_lines(ModelRc::default());
                    ui.set_stats("".into());
                    ui.set_filename(
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                            .into(),
                    );
                    state.source = Some(path);
                    state.message = Message::Converting;
                }
                Event::Converted(Ok(document)) => {
                    let (preview, truncated) = document.preview();
                    // One row per source line: the view only shapes what is on
                    // screen, so a long document costs no more than a short one.
                    ui.set_markdown_lines(ModelRc::new(VecModel::from(
                        preview.lines().map(SharedString::from).collect::<Vec<_>>(),
                    )));
                    ui.set_truncated(truncated);
                    ui.set_has_result(true);
                    ui.set_converting(false);
                    ui.set_busy(false);
                    state.message = if document.markdown.trim().is_empty() {
                        Message::EmptyResult
                    } else {
                        Message::Complete
                    };
                    state.result = Some(document);
                }
                Event::Converted(Err(error)) | Event::Finished(Err(error)) => {
                    ui.set_converting(false);
                    ui.set_busy(false);
                    state.message = Message::Error(error);
                }
                Event::Finished(Ok(message)) => {
                    ui.set_converting(false);
                    ui.set_busy(false);
                    state.message = message;
                }
            }
            refresh_text(&ui, &state);
        }
    });
    Ok(timer)
}
