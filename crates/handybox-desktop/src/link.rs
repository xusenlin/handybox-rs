//! Hand a URL to the desktop's default handler.
//!
//! The application still makes no network requests of its own; this only asks
//! the operating system to open the user's browser.

use anyhow::{Context, Result};
use std::process::{Command, Stdio};

/// Displayed in the status bar; the scheme is added when opening.
pub const REPOSITORY: &str = "github.com/xusenlin/handybox-rs";

pub fn open(url: &str) -> Result<()> {
    // Spawned, never awaited: the handler outlives this process and its output
    // belongs to the browser, not to us.
    let mut command = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("cmd");
        // The empty argument is `start`'s window title, without which a quoted
        // URL would be taken as the title instead of the address.
        cmd.args(["/C", "start", "", url]);
        cmd
    } else if cfg!(target_os = "macos") {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    } else {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("Could not open {url}"))?;
    Ok(())
}
