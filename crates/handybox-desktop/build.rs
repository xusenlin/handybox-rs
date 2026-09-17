fn main() {
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent-light".into()),
    )
    .expect("Slint UI compilation failed");

    // The Windows icon has to live in the executable's resource section: the
    // icon Slint sets at runtime reaches the window only, not Explorer, the
    // taskbar or a pinned shortcut. mingw's windres does this when cross
    // compiling. The resource also carries the version info that an unsigned
    // binary otherwise ships without.
    println!("cargo:rerun-if-changed=../../assets/app-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../assets/app-icon.ico")
            .compile()
            .expect("Embedding the Windows icon resource failed");
    }
}
