use anyhow::{Result, bail};

/// Keep winit's fallback-capable factory. The fallback happens
/// when a window/surface is created, not just when the backend is selected.
pub fn init() -> Result<()> {
    let renderer = match std::env::var("SLINT_BACKEND").as_deref() {
        Ok("winit-software" | "software") => Some("software"),
        Ok("winit-femtovg" | "femtovg") => Some("femtovg"),
        Ok("" | "winit") | Err(_) => None,
        Ok(other) => {
            bail!("Unsupported SLINT_BACKEND={other}. Use winit, winit-femtovg or winit-software.")
        }
    };
    slint::platform::set_platform(Box::new(
        i_slint_backend_winit::Backend::new_with_renderer_by_name(renderer)?,
    ))?;
    Ok(())
}
