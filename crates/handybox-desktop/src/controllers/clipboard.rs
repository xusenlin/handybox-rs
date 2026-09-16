//! Clipboard workspace: what passed through the clipboard, kept for the session.
//!
//! The list is the tool. Everything else is one of two things: putting an item
//! back where it came from, or deciding whether new ones arrive on their own.
//!
//! Collecting automatically is off until it is switched on, and the switch is
//! the only thing that starts it — a clipboard carries passwords as often as it
//! carries a paragraph, so a tool that keeps everything has to be asked to. The
//! items are held in memory and nothing here writes to disk; closing the window
//! is what empties the workspace.
//!
//! What is retained is the engine's own value, never rendered text, so a
//! language change re-states every line in place. The picture on show is the
//! exception in the other direction: it is pixels rather than words, and it is
//! built once, when the selection changes, rather than on every refresh.
use super::Shell;
use crate::{
    AppWindow, ClipItem, ClipboardState,
    locale::{Language, Message},
    worker::{Command, clipboard::Command as Job, clipboard::Event},
};
use handybox_core::tools::clipboard::{
    self as workspace, Content, History, Item, ItemId, Kind, PREVIEW_CHARS, Recorded,
};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

/// How often the caption under the selection re-states how long ago the item
/// was captured. Only that one line: rebuilding the list this often would move
/// it under the pointer for no reason.
const RESTATE: Duration = Duration::from_secs(15);

#[derive(Default)]
struct State {
    history: History,
    selected: Option<ItemId>,
    /// Which item's picture is currently in the view, so a refresh that changed
    /// nothing does not hand the renderer the same pixels again.
    shown: Option<ItemId>,
    /// What the switch is actually set to, as opposed to what it looks like: a
    /// watcher that could not start has to be taken back.
    watching: bool,
    restated: Option<Instant>,
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
        let view = ui.global::<ClipboardState>();

        let bound = shell.clone();
        view.on_capture(move || {
            bound.submit(Command::Clipboard(Job::Capture), Message::Capturing);
        });
        // The switch has already moved by the time this runs, so a refused
        // command has to put it back rather than leave it claiming to watch.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_watch(move |wanted| {
            if !bound.submit(Command::Clipboard(Job::Watch(wanted)), Message::Ready) {
                controller.show_watching();
            }
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_clear(move || {
            {
                let mut state = controller.state.borrow_mut();
                state.history.clear();
                state.selected = None;
            }
            controller.refresh(bound.language());
            // Worth saying out loud, because the button is next to a clipboard:
            // what was emptied is this list, not the system's own clipboard.
            bound.notify(Message::Cleared);
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_select(move |id| {
            controller.state.borrow_mut().selected = identify(id);
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_copy(move |id| {
            let Some(content) = controller.content(identify(id)) else {
                return;
            };
            bound.submit(Command::Clipboard(Job::Restore(content)), Message::Copying);
        });
        // A picture is the one kind that cannot leave through the clipboard
        // alone: pasting it needs somewhere that takes pictures, and a file is
        // the general answer. Text and paths already paste anywhere.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_save(move || {
            let Some(png) = controller.picture() else {
                return;
            };
            bound.submit(
                Command::Clipboard(Job::Save {
                    language: bound.language(),
                    png,
                }),
                Message::Saving,
            );
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_remove(move |id| {
            let Some(id) = identify(id) else { return };
            {
                let mut state = controller.state.borrow_mut();
                state.history.remove(id);
                if state.selected == Some(id) {
                    state.selected = None;
                }
            }
            controller.refresh(bound.language());
        });
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        match event {
            Event::Captured(Ok(content)) => {
                // A capture the user asked for points at what it captured, even
                // when that was already in the workspace.
                let recorded = self.record(content);
                self.state.borrow_mut().selected = Some(recorded.id());
                self.refresh(shell.language());
                shell.finish(Ok(match recorded {
                    Recorded::Added(_) => Message::Collected,
                    Recorded::Moved(_) => Message::AlreadyCollected,
                }));
            }
            Event::Captured(Err(failure)) => shell.finish(Err(failure)),
            Event::Noticed(content) => {
                // Collected on its own, so it takes nothing over: the item on
                // show stays on show, and the notice surface stays quiet.
                let recorded = self.record(content);
                let mut state = self.state.borrow_mut();
                if state.selected.is_none() {
                    state.selected = Some(recorded.id());
                }
                drop(state);
                self.refresh(shell.language());
            }
            Event::Watching(watching) => {
                self.state.borrow_mut().watching = watching;
                self.show_watching();
                if watching {
                    // Whatever is on the clipboard right now is the first thing
                    // someone switching this on would expect to see collected.
                    shell.dispatch(Command::Clipboard(Job::Notice));
                }
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ClipboardState>();
        let mut state = self.state.borrow_mut();
        let now = Instant::now();
        state.restated = Some(now);

        view.set_items(ModelRc::new(VecModel::from(
            state
                .history
                .items()
                .iter()
                .map(|item| present(item, lang))
                .collect::<Vec<_>>(),
        )));
        view.set_has_items(!state.history.is_empty());
        view.set_stats(stats(&state.history, lang).into());

        let selected = state.selected.filter(|id| state.history.get(*id).is_some());
        state.selected = selected;
        view.set_selected(selected.map_or(-1, |id| id as i32));
        // Settled before the item is borrowed out of the history, so that
        // looking at it and remembering it are not the same borrow.
        let fresh = state.shown != selected;
        state.shown = selected;
        let Some(item) = selected.and_then(|id| state.history.get(id)) else {
            view.set_has_detail(false);
            view.set_detail_kind(0);
            view.set_detail_title(SharedString::new());
            view.set_detail_caption(SharedString::new());
            view.set_detail_lines(ModelRc::default());
            view.set_detail_image(Image::default());
            view.set_status(SharedString::new());
            return;
        };
        view.set_has_detail(true);
        view.set_detail_kind(kind(item.kind()));
        view.set_detail_title(title(item, lang).into());
        view.set_detail_caption(caption(item, now, lang).into());
        view.set_detail_lines(ModelRc::new(VecModel::from(lines(&item.content))));
        view.set_status(status(item, lang).into());
        // The pixels are the worker's; handing them over again on a language
        // change would be a copy for nothing.
        if fresh {
            let (picture, width, height) = match &item.content {
                Content::Image(picture) => (
                    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                        &picture.thumbnail.rgba,
                        picture.thumbnail.width,
                        picture.thumbnail.height,
                    )),
                    picture.thumbnail.width as f32,
                    picture.thumbnail.height as f32,
                ),
                _ => (Image::default(), 0.0, 0.0),
            };
            view.set_detail_image(picture);
            view.set_detail_width(width);
            view.set_detail_height(height);
        }
    }

    /// Keep "a minute ago" true without rebuilding the list under the pointer.
    pub fn tick(&self, shell: &Shell) {
        let Some(ui) = self.ui.upgrade() else { return };
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        if state
            .restated
            .is_some_and(|last| now.saturating_duration_since(last) < RESTATE)
        {
            return;
        }
        state.restated = Some(now);
        let Some(item) = state.selected.and_then(|id| state.history.get(id)) else {
            return;
        };
        ui.global::<ClipboardState>()
            .set_detail_caption(caption(item, now, shell.language()).into());
    }

    fn record(&self, content: Content) -> Recorded {
        self.state
            .borrow_mut()
            .history
            .record(content, Instant::now())
    }

    /// What an item holds, for putting it back on the clipboard.
    fn content(&self, id: Option<ItemId>) -> Option<Content> {
        let state = self.state.borrow();
        state.history.get(id?).map(|item| item.content.clone())
    }

    /// The selected picture as the bytes that were collected, if the selection
    /// is a picture at all.
    fn picture(&self) -> Option<Vec<u8>> {
        let state = self.state.borrow();
        match &state.history.get(state.selected?)?.content {
            Content::Image(picture) => Some(picture.png.clone()),
            _ => None,
        }
    }

    /// Put the switch back in step with the watcher it stands for.
    fn show_watching(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.global::<ClipboardState>()
            .set_watching(self.state.borrow().watching);
    }
}

/// A row's id as the workspace knows it. The page sends -1 when nothing is
/// selected, which is not an item and never was one.
fn identify(id: i32) -> Option<ItemId> {
    (id > 0).then_some(id as ItemId)
}

fn kind(kind: Kind) -> i32 {
    match kind {
        Kind::Text => 0,
        Kind::Image => 1,
        Kind::Files => 2,
    }
}

fn label(kind: Kind, lang: Language) -> &'static str {
    match kind {
        Kind::Text => lang.text("Text", "文本"),
        Kind::Image => lang.text("Image", "图片"),
        Kind::Files => lang.text("Files", "文件"),
    }
}

fn present(item: &Item, lang: Language) -> ClipItem {
    let summary = item.content.summary();
    ClipItem {
        id: item.id as i32,
        kind: kind(item.kind()),
        label: label(item.kind(), lang).into(),
        // A text of nothing but spaces and newlines has no line to show, and
        // saying so is more use than an empty row.
        summary: if summary.is_empty() {
            SharedString::from(lang.text("Whitespace only", "仅包含空白字符"))
        } else {
            summary.into()
        },
        detail: detail(&item.content, lang).into(),
    }
}

/// The small line under a row: the one measure that says most about the item.
fn detail(content: &Content, lang: Language) -> String {
    match content {
        Content::Text(text) => {
            let characters = workspace::stats(text).characters;
            format!("{characters} {}", characters_word(characters, lang))
        }
        Content::Image(_) => size(content.size()),
        Content::Files(paths) => format!("{} {}", paths.len(), files_word(paths.len(), lang)),
    }
}

fn title(item: &Item, lang: Language) -> String {
    label(item.kind(), lang).to_owned()
}

/// Everything about the item on show, in one line: how long ago it arrived, and
/// what it amounts to.
fn caption(item: &Item, now: Instant, lang: Language) -> String {
    let mut parts = vec![age(item, now, lang)];
    match &item.content {
        Content::Text(text) => {
            let stats = workspace::stats(text);
            parts.push(format!("{} {}", stats.lines, lines_word(stats.lines, lang)));
            parts.push(format!(
                "{} {}",
                stats.characters,
                characters_word(stats.characters, lang)
            ));
            parts.push(size(stats.bytes));
        }
        Content::Image(picture) => {
            parts.push(format!("{} × {}", picture.width, picture.height));
            parts.push(size(picture.png.len()));
        }
        Content::Files(paths) => {
            parts.push(format!("{} {}", paths.len(), files_word(paths.len(), lang)));
        }
    }
    parts.join("  ·  ")
}

/// What copying this item would actually put on the clipboard. Worth stating
/// for file references, where it is the files themselves rather than their
/// names — paste one in a file manager and it is a copy of the file.
fn status(item: &Item, lang: Language) -> &'static str {
    match item.kind() {
        Kind::Text => lang.text(
            "Copy puts this text back on your clipboard, ready to paste.",
            "点击复制即可把这段文本放回剪贴板，随时可以粘贴。",
        ),
        Kind::Image => lang.text(
            "Copy puts the original picture back on your clipboard, not the preview shown here.",
            "点击复制放回剪贴板的是原始图片，而不是这里显示的预览图。",
        ),
        Kind::Files => lang.text(
            "Copy puts the file references back, so pasting in a file manager pastes the files themselves.",
            "点击复制放回的是文件引用，在文件管理器中粘贴得到的是文件本身。",
        ),
    }
}

/// The lines the panel shows: the text as it was copied, or one path per line.
/// Bounded like every other preview in the application — copying back always
/// uses the whole item, however long it is.
fn lines(content: &Content) -> Vec<SharedString> {
    match content {
        Content::Text(text) => {
            let shown = match text.char_indices().nth(PREVIEW_CHARS) {
                Some((at, _)) => &text[..at],
                None => text.as_str(),
            };
            shown.lines().map(SharedString::from).collect()
        }
        Content::Files(paths) => paths
            .iter()
            .map(|path| SharedString::from(path.display().to_string()))
            .collect(),
        Content::Image(_) => Vec::new(),
    }
}

fn stats(history: &History, lang: Language) -> String {
    format!(
        "{} {}  ·  {}",
        history.len(),
        items_word(history.len(), lang),
        size(history.size())
    )
}

/// How long ago an item arrived, to the precision anyone reading it cares
/// about: a clipboard workspace is minutes old, not dated.
fn age(item: &Item, now: Instant, lang: Language) -> String {
    let seconds = item.age(now).as_secs();
    if seconds < 45 {
        return lang.text("Just now", "刚刚").to_owned();
    }
    if seconds < 3600 {
        let minutes = (seconds / 60).max(1);
        return format!(
            "{minutes} {}",
            if minutes == 1 {
                lang.text("minute ago", "分钟前")
            } else {
                lang.text("minutes ago", "分钟前")
            }
        );
    }
    let hours = seconds / 3600;
    format!(
        "{hours} {}",
        if hours == 1 {
            lang.text("hour ago", "小时前")
        } else {
            lang.text("hours ago", "小时前")
        }
    )
}

/// Byte counts are shown to people, not parsed: one decimal, and the unit that
/// keeps the number readable. A workspace of screenshots passes a megabyte fast.
fn size(bytes: usize) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else {
        super::kib(bytes)
    }
}

fn items_word(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("item", "条内容")
    } else {
        lang.text("items", "条内容")
    }
}

fn files_word(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("file", "个文件")
    } else {
        lang.text("files", "个文件")
    }
}

fn lines_word(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("line", "行")
    } else {
        lang.text("lines", "行")
    }
}

fn characters_word(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("character", "个字符")
    } else {
        lang.text("characters", "个字符")
    }
}
