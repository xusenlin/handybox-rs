/// macOS window chrome that Slint cannot set from the component tree:
/// the Dock icon, which a Window's icon property does not provide, and the
/// system appearance, which otherwise leaves a dark title bar above the
/// application's light theme.
#[cfg(target_os = "macos")]
pub fn install() -> anyhow::Result<()> {
    use anyhow::Context;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread = MainThreadMarker::new().context("App icon must be set on the main thread")?;
    let app = NSApplication::sharedApplication(main_thread);

    // A Window icon is ignored by macOS. Set the Dock icon explicitly, including
    // when running the unbundled development executable with `cargo run`.
    let bytes = NSData::with_bytes(include_bytes!("../../../assets/macos-icon.png"));
    let image =
        NSImage::initWithData(NSImage::alloc(), &bytes).context("Could not decode app logo")?;
    // SAFETY: Supply a valid non-null NSImage, on the application's main thread.
    unsafe { app.setApplicationIconImage(Some(&image)) };

    // The palette in Theme is light only, so follow it instead of the system.
    // This covers the menu bar and native dialogs; the window itself is handled
    // by `use_light_titlebar` once winit has created it.
    // SAFETY: Reading an AppKit string constant exported by the framework.
    let aqua = unsafe { NSAppearanceNameAqua };
    app.setAppearance(NSAppearance::appearanceNamed(aqua).as_deref());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn install() -> anyhow::Result<()> {
    // Other desktop platforms use AppWindow.icon from the Slint component.
    Ok(())
}

/// Keep the title bar light, matching `Theme`'s light-only palette; in Dark Mode
/// macOS would otherwise draw a near-black title bar above a light window.
///
/// Slint's own `set_color_scheme` cannot do this: its winit backend compiles the
/// theme call out on Apple targets (`use_winit_theme`), leaving the decoration to
/// follow the system. The window must already exist, so call this after `show`.
pub fn use_light_titlebar(window: &slint::Window) {
    #[cfg(target_os = "macos")]
    {
        use i_slint_backend_winit::{WinitWindowAccessor, winit::window::Theme};
        window.with_winit_window(|winit_window| winit_window.set_theme(Some(Theme::Light)));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}
