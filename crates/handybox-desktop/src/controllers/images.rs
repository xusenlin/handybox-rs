//! Image studio: a folder of pictures, one of them on show, and the settings
//! that would be applied to everything ticked.
//!
//! Two selections, because they answer different questions. What is *ticked* is
//! what a batch would write; what is *highlighted* is what the page is showing.
//! They are deliberately separate: someone can move through the folder without
//! changing what they are about to export.
//!
//! What is retained is the engine's own values, never rendered text, so a
//! language change re-states every line in place and both selections survive it.
use super::Shell;
use crate::{
    AppWindow, DetailRow, ImageRow, ImagesState,
    locale::{Language, Message, size},
    worker::{
        Command,
        images::{Command as Job, Event},
    },
};
use handybox_core::tools::images::{Detail, Format, Listing, Picture, Recipe, Tag};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
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

/// What the three quality buttons mean. JPEG quality is a number nobody has an
/// intuition for; these are the three answers people actually want.
const QUALITIES: [u8; 3] = [70, 85, 95];

#[derive(Default)]
struct State {
    listing: Option<Listing>,
    /// Indices into the listing: what a batch would write.
    ticked: BTreeSet<usize>,
    /// The one being shown, which is not the same question.
    shown: Option<usize>,
    picture: Option<Arc<Picture>>,
    recipe: Recipe,
    /// Where a batch has got to, while one is running.
    progress: Option<(usize, usize)>,
    /// A picture that was dropped on the window: it is shown as soon as the
    /// folder it belongs to has been listed.
    wanted: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: Rc<RefCell<State>>,
    /// Raised to stop a batch between pictures.
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
        let view = ui.global::<ImagesState>();

        let bound = shell.clone();
        view.on_choose(move || {
            bound.submit(
                Command::Images(Job::Choose(bound.language())),
                Message::Picking,
            );
        });
        // Showing a picture and ticking it are different acts on the same row.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_show(move |index| controller.display(&bound, index));
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_toggle(move |index| {
            if controller.busy() {
                return;
            }
            if index >= 0 {
                let mut state = controller.state.borrow_mut();
                if state
                    .listing
                    .as_ref()
                    .is_none_or(|listing| (index as usize) >= listing.entries.len())
                {
                    return;
                }
                let index = index as usize;
                if !state.ticked.remove(&index) {
                    state.ticked.insert(index);
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
                let all: BTreeSet<usize> = state
                    .listing
                    .as_ref()
                    .map(|listing| (0..listing.entries.len()).collect())
                    .unwrap_or_default();
                // The same button clears when everything is already ticked:
                // there is only one thing left to want at that point.
                state.ticked = if state.ticked == all {
                    BTreeSet::new()
                } else {
                    all
                };
            }
            controller.refresh(bound.language());
        });
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_settings_changed(move || {
            controller.adopt_settings();
            controller.refresh(bound.language());
        });
        // The batch reads, renders and writes one picture at a time, so nothing
        // has to be held in memory for the whole folder.
        let (bound, controller) = (shell.clone(), self.clone());
        view.on_export(move || {
            if controller.busy() {
                return;
            }
            controller.adopt_settings();
            let job = {
                let state = controller.state.borrow();
                let sources: Vec<PathBuf> = match &state.listing {
                    Some(listing) => state
                        .ticked
                        .iter()
                        .filter_map(|index| listing.entries.get(*index))
                        .map(|entry| entry.path.clone())
                        .collect(),
                    None => Vec::new(),
                };
                (!sources.is_empty()).then(|| Job::Batch {
                    language: bound.language(),
                    sources,
                    recipe: state.recipe,
                    cancel: controller.cancel.clone(),
                })
            };
            if let Some(job) = job {
                controller.cancel.store(false, Ordering::Relaxed);
                // Deliberately not marked as working yet: the job opens a
                // folder dialog first, and a cancelled dialog leaves nothing
                // running. The first progress report is what starts the bar.
                bound.submit(Command::Images(job), Message::Exporting);
            }
        });
        let controller = self.clone();
        view.on_stop(move || controller.cancel.store(true, Ordering::Relaxed));
    }

    /// Adopt a path the user already chose. A folder is the input; a picture is
    /// taken as the folder it sits in, with that picture on show, because that
    /// is what dropping one of forty photographs means.
    pub fn open(&self, shell: &Shell, path: PathBuf) {
        shell.navigate("images");
        let (folder, wanted) = if path.is_dir() {
            (path, None)
        } else {
            match path.parent() {
                Some(parent) => (parent.to_owned(), Some(path.clone())),
                None => (path, None),
            }
        };
        self.state.borrow_mut().wanted = wanted.map(|path| path.canonicalize().unwrap_or(path));
        self.adopt(shell, folder);
    }

    pub fn handle(&self, shell: &Shell, event: Event) {
        match event {
            Event::Chosen(path) => {
                // The picker's operation ends here; reading the folder takes
                // the busy state on its own.
                shell.finish(Ok(Message::Ready));
                self.adopt(shell, path);
            }
            Event::Listed(Ok(listing)) => {
                self.working(false);
                let empty = listing.entries.is_empty();
                let wanted = {
                    let mut state = self.state.borrow_mut();
                    let wanted = state.wanted.take().and_then(|path| {
                        listing.entries.iter().position(|entry| entry.path == path)
                    });
                    state.listing = Some(listing);
                    state.ticked.clear();
                    state.shown = None;
                    state.picture = None;
                    wanted
                };
                self.refresh(shell.language());
                shell.finish(Ok(if empty {
                    Message::NoPictures
                } else {
                    Message::Ready
                }));
                // The picture that was dropped, or simply the first one: a page
                // that listed a folder and showed nothing would be a page that
                // asked for one more click to say anything at all.
                if !empty {
                    self.display(shell, wanted.unwrap_or(0) as i32);
                }
            }
            Event::Listed(Err(failure)) => {
                self.working(false);
                self.reset(shell);
                shell.finish(Err(failure));
            }
            Event::Loaded(Ok(picture)) => {
                self.working(false);
                self.show(&picture);
                {
                    let mut state = self.state.borrow_mut();
                    state.picture = Some(picture);
                }
                self.refresh(shell.language());
                shell.finish(Ok(Message::Ready));
            }
            Event::Loaded(Err(failure)) => {
                self.working(false);
                {
                    let mut state = self.state.borrow_mut();
                    state.picture = None;
                }
                self.blank();
                self.refresh(shell.language());
                shell.finish(Err(failure));
            }
            Event::Progress { done, total } => {
                self.state.borrow_mut().progress = Some((done, total));
                self.exporting(true);
                self.refresh(shell.language());
            }
            Event::Batched => {
                self.exporting(false);
                self.state.borrow_mut().progress = None;
                self.refresh(shell.language());
            }
        }
    }

    /// Re-render every string this tool derives from its state.
    pub fn refresh(&self, lang: Language) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ImagesState>();
        let state = self.state.borrow();
        view.set_format(match state.recipe.format {
            Format::Png => 0,
            Format::Jpeg => 1,
            Format::Tiff => 2,
            Format::Bmp => 3,
        });
        view.set_lossless(state.recipe.format.lossless());
        view.set_optimize(state.recipe.optimize);
        view.set_quality(
            QUALITIES
                .iter()
                .position(|quality| *quality == state.recipe.quality)
                .unwrap_or(1) as i32,
        );
        view.set_folder(match &state.listing {
            Some(listing) => listing.folder.display().to_string().into(),
            None => SharedString::new(),
        });
        view.set_detail(SharedString::from(match &state.listing {
            Some(_) => lang.text(
                "Reads only. Nothing is written until you export, and nothing is ever replaced.",
                "只读取，不改动。只有点击导出才会写文件，而且从不覆盖已有文件。",
            ),
            None => lang.text(
                "Choose a folder of pictures, or drop one on the window",
                "选择存放图片的文件夹，或把文件夹拖到窗口上",
            ),
        }));

        let Some(listing) = &state.listing else {
            view.set_rows(ModelRc::default());
            view.set_has_folder(false);
            view.set_has_picture(false);
            view.set_details(ModelRc::default());
            view.set_has_details(false);
            view.set_filename(SharedString::new());
            view.set_facts(ModelRc::default());
            view.set_summary(SharedString::new());
            view.set_selection(SharedString::new());
            view.set_has_selection(false);
            view.set_status(status(None, None, lang).into());
            return;
        };
        view.set_has_folder(!listing.entries.is_empty());
        view.set_rows(ModelRc::new(VecModel::from(rows(
            listing,
            &state.ticked,
            state.shown,
        ))));
        view.set_summary(summary(listing, lang).into());
        view.set_has_selection(!state.ticked.is_empty());
        view.set_selection(selection(listing, &state.ticked, state.progress, lang).into());
        view.set_filename(
            state
                .shown
                .and_then(|index| listing.entries.get(index))
                .map(|entry| entry.name.as_str())
                .unwrap_or("")
                .into(),
        );
        let mut facts = Vec::new();
        let mut fact = |label: &str, value: String| {
            facts.push(DetailRow {
                label: label.into(),
                value: value.into(),
            })
        };
        if let Some(picture) = &state.picture {
            fact(
                lang.text("Dimensions", "尺寸"),
                format!("{} × {}", picture.width, picture.height),
            );
            fact(lang.text("File size", "文件大小"), size(picture.bytes));
            fact(lang.text("Format", "格式"), picture.format.to_owned());
            fact(lang.text("Colour", "色彩"), picture.colour.to_owned());
        } else if let Some(entry) = state.shown.and_then(|index| listing.entries.get(index)) {
            fact(
                lang.text("Dimensions", "尺寸"),
                format!("{} × {}", entry.width, entry.height),
            );
            fact(lang.text("File size", "文件大小"), size(entry.bytes));
        }
        view.set_facts(ModelRc::new(VecModel::from(facts)));
        view.set_has_picture(state.picture.is_some());
        let details = state
            .picture
            .as_ref()
            .map(|picture| rows_of(picture, lang))
            .unwrap_or_default();
        view.set_has_details(!details.is_empty());
        view.set_details(ModelRc::new(VecModel::from(details)));
        view.set_status(status(Some(listing), state.picture.as_deref(), lang).into());
    }

    /// Take a folder as the input and read it.
    fn adopt(&self, shell: &Shell, folder: PathBuf) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.global::<ImagesState>()
            .set_folder(folder.display().to_string().into());
        {
            let mut state = self.state.borrow_mut();
            state.listing = None;
            state.ticked.clear();
            state.shown = None;
            state.picture = None;
        }
        self.blank();
        self.refresh(shell.language());
        let accepted = shell.submit(Command::Images(Job::Scan(folder)), Message::Opening);
        self.working(accepted);
    }

    /// Put one of the folder's pictures on show. Ticking is left alone: what is
    /// being looked at and what would be written are different questions.
    fn display(&self, shell: &Shell, index: i32) {
        if self.busy() || index < 0 {
            return;
        }
        if self.state.borrow().shown == Some(index as usize)
            && self.state.borrow().picture.is_some()
        {
            return;
        }
        let path = {
            let mut state = self.state.borrow_mut();
            let Some(listing) = &state.listing else {
                return;
            };
            let Some(entry) = listing.entries.get(index.max(0) as usize) else {
                return;
            };
            let path = entry.path.clone();
            state.shown = Some(index.max(0) as usize);
            state.picture = None;
            path
        };
        self.blank();
        self.refresh(shell.language());
        let accepted = shell.submit(Command::Images(Job::Load(path)), Message::Opening);
        self.working(accepted);
    }

    /// Read the controls back into the recipe. They are the page's own state
    /// between presses; this is where they become the thing that is written.
    fn adopt_settings(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ImagesState>();
        let mut state = self.state.borrow_mut();
        state.recipe.format = match view.get_format() {
            1 => Format::Jpeg,
            2 => Format::Tiff,
            3 => Format::Bmp,
            _ => Format::Png,
        };
        state.recipe.quality = QUALITIES
            .get(view.get_quality().max(0) as usize)
            .copied()
            .unwrap_or(85);
        state.recipe.optimize = view.get_optimize();
        // An empty field is every picture's own width, which is also what
        // converting alone means. Anything that is not a number is refused
        // during export rather than silently ignored.
        state.recipe.width = match view.get_width().trim() {
            "" => None,
            text => Some(text.parse::<u32>().unwrap_or(0)),
        };
    }

    /// Put the page back to having no folder.
    fn reset(&self, shell: &Shell) {
        {
            let mut state = self.state.borrow_mut();
            state.listing = None;
            state.ticked.clear();
            state.shown = None;
            state.picture = None;
            state.progress = None;
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ImagesState>().set_folder(SharedString::new());
        }
        self.blank();
        self.refresh(shell.language());
    }

    /// The picture as an image the renderer can take. Built once, when it is
    /// opened: the pixels are the engine's, and handing them over again on
    /// every language change would be a copy for nothing.
    fn show(&self, picture: &Picture) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<ImagesState>();
        view.set_preview(Image::from_rgba8(
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &picture.preview.rgba,
                picture.preview.width,
                picture.preview.height,
            ),
        ));
    }

    /// Take the picture off screen while the next one is being read.
    fn blank(&self) {
        if let Some(ui) = self.ui.upgrade() {
            let view = ui.global::<ImagesState>();
            view.set_preview(Image::default());
            view.set_has_picture(false);
        }
    }

    fn busy(&self) -> bool {
        self.ui.upgrade().is_none_or(|ui| ui.get_busy())
    }

    fn working(&self, working: bool) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ImagesState>().set_working(working);
        }
    }

    /// Whether a batch is running. Separate from `working`, because the button
    /// that stops one must not appear while a single picture is being opened.
    fn exporting(&self, exporting: bool) {
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<ImagesState>().set_exporting(exporting);
        }
    }
}

/// The folder as rows: what each picture is, whether it would be written, and
/// which one is on show.
fn rows(listing: &Listing, ticked: &BTreeSet<usize>, shown: Option<usize>) -> Vec<ImageRow> {
    listing
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| ImageRow {
            name: entry.name.as_str().into(),
            detail: format!(
                "{} × {}  ·  {}",
                entry.width,
                entry.height,
                size(entry.bytes)
            )
            .into(),
            ticked: ticked.contains(&index),
            shown: shown == Some(index),
            index: index as i32,
        })
        .collect()
}

/// What the picture on show says about itself, in the order someone reads it.
fn rows_of(picture: &Picture, lang: Language) -> Vec<DetailRow> {
    picture
        .details
        .iter()
        .map(|detail| DetailRow {
            label: label(detail, lang).into(),
            value: detail.value.as_str().into(),
        })
        .collect()
}

fn label(detail: &Detail, lang: Language) -> &'static str {
    match detail.tag {
        Tag::Camera => lang.text("Camera", "相机"),
        Tag::Lens => lang.text("Lens", "镜头"),
        Tag::Taken => lang.text("Taken", "拍摄时间"),
        Tag::Exposure => lang.text("Exposure", "快门"),
        Tag::Aperture => lang.text("Aperture", "光圈"),
        Tag::Iso => lang.text("ISO", "ISO"),
        Tag::FocalLength => lang.text("Focal length", "焦距"),
        Tag::Software => lang.text("Software", "处理软件"),
        Tag::Location => lang.text("Location", "拍摄位置"),
    }
}

fn summary(listing: &Listing, lang: Language) -> String {
    let bytes: u64 = listing.entries.iter().map(|entry| entry.bytes).sum();
    let mut line = format!(
        "{} {}  ·  {}",
        listing.entries.len(),
        pictures(listing.entries.len(), lang),
        size(bytes)
    );
    if listing.left_out > 0 {
        line.push_str(&format!(
            "  ·  {} {} {}",
            lang.text("and", "另有"),
            listing.left_out,
            lang.text("more that this list does not hold", "张未列出")
        ));
    }
    line
}

/// What would be written, or where a batch has got to.
fn selection(
    listing: &Listing,
    ticked: &BTreeSet<usize>,
    progress: Option<(usize, usize)>,
    lang: Language,
) -> String {
    if let Some((done, total)) = progress {
        return format!("{} {done} / {total}…", lang.text("Exporting", "正在导出"));
    }
    if ticked.is_empty() {
        return lang
            .text(
                "Tick the pictures to export. Nothing is ticked yet.",
                "勾选要导出的图片；目前还没有勾选任何一张。",
            )
            .to_owned();
    }
    let bytes: u64 = ticked
        .iter()
        .filter_map(|index| listing.entries.get(*index))
        .map(|entry| entry.bytes)
        .sum();
    format!(
        "{} {} {}  ·  {}",
        lang.text("Ticked", "已勾选"),
        ticked.len(),
        pictures(ticked.len(), lang),
        size(bytes)
    )
}

/// The sentence under the page: what to do next, or what to know before doing
/// it.
fn status(listing: Option<&Listing>, picture: Option<&Picture>, lang: Language) -> String {
    let Some(listing) = listing else {
        return lang
            .text(
                "Everything happens on your device, and an exported picture carries no EXIF at all.",
                "所有处理都在本机完成；导出的图片不含任何 EXIF 信息。",
            )
            .to_owned();
    };
    if listing.entries.is_empty() {
        return lang
            .text(
                "No pictures in this folder. Only the folder itself is read — what is inside its subfolders stays there.",
                "这个文件夹里没有图片。只读取该文件夹本身，子文件夹中的内容不参与。",
            )
            .to_owned();
    }
    if picture.is_some_and(|picture| !picture.details.is_empty()) {
        return lang
            .text(
                "EXIF metadata is shown on the right and is removed from exported images.",
                "EXIF 信息列在右侧；导出时会移除这些信息。",
            )
            .to_owned();
    }
    lang.text(
        "Export applies the current settings to every ticked picture.",
        "导出会把当前设置应用到所有勾选的图片。",
    )
    .to_owned()
}

fn pictures(count: usize, lang: Language) -> &'static str {
    if count == 1 {
        lang.text("picture", "张图片")
    } else {
        lang.text("pictures", "张图片")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPlatform;
    impl slint::platform::Platform for TestPlatform {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(slint::platform::software_renderer::MinimalSoftwareWindow::new(Default::default()))
        }
    }

    #[test]
    fn loading_keeps_selection_stable() {
        slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
        let ui = AppWindow::new().unwrap();
        let (commands, incoming) = std::sync::mpsc::sync_channel(4);
        let shell = Shell {
            ui: ui.as_weak(),
            commands,
            state: Default::default(),
        };
        let controller = Controller::new(&ui);
        controller.bind(&shell);
        controller.state.borrow_mut().listing = Some(Listing {
            folder: PathBuf::from("/pictures"),
            entries: ["one.png", "two.png"]
                .into_iter()
                .map(|name| handybox_core::tools::images::Entry {
                    name: name.to_owned(),
                    path: PathBuf::from(name),
                    width: 100,
                    height: 200,
                    bytes: 1000,
                })
                .collect(),
            left_out: 0,
        });
        controller.display(&shell, 0);
        assert!(ui.get_busy());
        assert!(matches!(
            incoming.try_recv().unwrap(),
            Command::Images(Job::Load(_))
        ));
        ui.global::<ImagesState>().invoke_show(1);
        ui.global::<ImagesState>().invoke_toggle(1);
        assert_eq!(controller.state.borrow().shown, Some(0));
        assert!(controller.state.borrow().ticked.is_empty());
        assert!(incoming.try_recv().is_err());
        assert_eq!(ui.global::<ImagesState>().get_filename(), "one.png");
        assert!(!ui.global::<ImagesState>().get_has_picture());
    }
}
