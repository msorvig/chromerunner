//! Browser and driver process launcher.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use tempfile::TempDir;
use tracing::{debug, info};

use crate::error::{Error, Result};

/// RAII guard that kills the child process on drop.
pub struct ChildGuard {
    process: Child,
    _temp_dir: Option<TempDir>,
}

impl ChildGuard {
    pub fn kill(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Find a free TCP port on localhost.
pub fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    debug!(port, "Found free port");
    Ok(port)
}

// ---------------------------------------------------------------------------
// Chrome
// ---------------------------------------------------------------------------

#[cfg(feature = "chrome")]
pub fn launch_chrome(port: u16, headless: bool) -> Result<ChildGuard> {
    let bin = find_chrome().ok_or_else(|| Error::LaunchFailed("Chrome not found".into()))?;
    info!(path = %bin.display(), port, headless, "Launching Chrome");
    let user_data_dir = TempDir::new()?;

    let mut cmd = Command::new(&bin);
    cmd.arg(format!("--remote-debugging-port={}", port))
        .arg(format!("--user-data-dir={}", user_data_dir.path().display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-default-apps")
        .arg("--disable-extensions")
        .arg("--disable-sync")
        .arg("--disable-translate")
        .arg("--metrics-recording-only")
        .arg("--mute-audio")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if headless {
        cmd.arg("--headless=new");
    }
    cmd.arg("about:blank");

    let process = cmd
        .spawn()
        .map_err(|e| Error::LaunchFailed(format!("{}: {}", bin.display(), e)))?;

    info!(pid = process.id(), "Chrome process started");
    Ok(ChildGuard {
        process,
        _temp_dir: Some(user_data_dir),
    })
}

#[cfg(feature = "chrome")]
fn find_chrome() -> Option<PathBuf> {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ];
    for p in &candidates {
        if std::path::Path::new(p).exists() {
            debug!(path = p, "Found Chrome");
            return Some(PathBuf::from(p));
        }
    }
    find_on_path(&[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ])
}

// ---------------------------------------------------------------------------
// Firefox (geckodriver)
// ---------------------------------------------------------------------------

#[cfg(feature = "firefox")]
pub fn launch_geckodriver(port: u16, ws_port: u16, headless: bool) -> Result<ChildGuard> {
    let bin = find_on_path(&["geckodriver"])
        .ok_or_else(|| Error::LaunchFailed("geckodriver not found on PATH".into()))?;
    info!(path = %bin.display(), port, ws_port, headless, "Launching geckodriver");

    let mut cmd = Command::new(&bin);
    cmd.arg("--host").arg("127.0.0.1");
    cmd.arg("--port").arg(port.to_string());
    cmd.arg("--websocket-port").arg(ws_port.to_string());
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null());

    let _ = headless; // handled via capabilities

    let process = cmd
        .spawn()
        .map_err(|e| Error::LaunchFailed(format!("{}: {}", bin.display(), e)))?;

    info!(pid = process.id(), "geckodriver process started");
    Ok(ChildGuard {
        process,
        _temp_dir: None,
    })
}

// ---------------------------------------------------------------------------
// Safari (safaridriver)
// ---------------------------------------------------------------------------

#[cfg(feature = "safari")]
pub fn launch_safaridriver(port: u16) -> Result<ChildGuard> {
    let bin = PathBuf::from("/usr/bin/safaridriver");
    if !bin.exists() {
        return Err(Error::LaunchFailed("safaridriver not found at /usr/bin/safaridriver".into()));
    }
    info!(port, "Launching safaridriver");

    let mut cmd = Command::new(&bin);
    cmd.arg("--port").arg(port.to_string());
    cmd.stdout(Stdio::null())
        .stderr(Stdio::null());

    let process = cmd
        .spawn()
        .map_err(|e| Error::LaunchFailed(format!("safaridriver: {}", e)))?;

    info!(pid = process.id(), "safaridriver process started");
    Ok(ChildGuard {
        process,
        _temp_dir: None,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Ok(output) = Command::new("which").arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    debug!(name, path, "Found on PATH");
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    None
}
