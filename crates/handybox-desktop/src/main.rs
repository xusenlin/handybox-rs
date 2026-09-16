#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod controllers;
mod link;
mod locale;
mod macos;
mod renderer;
mod settings;
mod worker;

use slint::ComponentHandle;
slint::include_modules!();

fn main() {
    if let Err(error) = run() {
        eprintln!("HandyBox: {error:#}");
        rfd::MessageDialog::new()
            .set_title(
                settings::load_language().text("HandyBox could not start", "HandyBox 无法启动"),
            )
            .set_description(format!("{error:#}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut files = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == "--help" || arg == "-h" {
            println!(
                "HandyBox — A fast, offline-first desktop utility toolbox.\n\nUsage: handybox [file|folder]\n       handybox <original> <changed>\n\nDrop or open a document to convert it to Markdown.\nAn image opens in the barcode reader, a .json in the workbench, an .age file to be decrypted,\na .zip or .7z to be looked inside.\nA folder opens in Disk cleanup and is scanned.\nTwo paths are compared side by side in the text diff tool.\nUse SLINT_BACKEND=winit-software to force CPU rendering."
            );
            return Ok(());
        }
        anyhow::ensure!(files.len() < 2, "Expected at most two paths");
        files.push(std::path::PathBuf::from(arg));
    }
    renderer::init()?;
    macos::install()?;
    let ui = AppWindow::new()?;
    // The proportional family follows the selected language and is owned by the
    // controller; only the monospace family is fixed here.
    ui.global::<Theme>().set_mono_family(
        if cfg!(target_os = "macos") {
            "Menlo"
        } else if cfg!(target_os = "windows") {
            "Consolas"
        } else {
            "DejaVu Sans Mono"
        }
        .into(),
    );
    let _events = controllers::bind(&ui, files)?;
    // Show before running the loop: the title bar appearance needs the native
    // window, which only exists once the component is shown.
    ui.show()?;
    macos::use_light_titlebar(ui.window());
    slint::run_event_loop()?;
    Ok(())
}
