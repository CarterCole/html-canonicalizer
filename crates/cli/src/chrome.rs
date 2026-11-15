//! Finding and launching a headless Chrome.

use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

pub struct Chrome {
    child: Child,
    pub ws_url: String,
    // Dropped after the process is killed.
    _profile: TempDir,
}

pub fn find(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Ok(p.to_path_buf());
        }
        bail!("no Chrome at {}", p.display());
    }
    for var in ["CANON_CHROME", "CHROME_PATH", "CHROME"] {
        if let Ok(v) = std::env::var(var) {
            let p = PathBuf::from(v);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    let fixed: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ]
    } else {
        &[]
    };
    for p in fixed {
        if Path::new(p).exists() {
            return Ok(PathBuf::from(p));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome", "microsoft-edge"] {
                let p = dir.join(name);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
    }
    Err(anyhow!("no Chrome or Chromium found; pass --chrome <path> or set CANON_CHROME"))
}

impl Chrome {
    pub fn launch(path: &Path, sandbox: bool) -> Result<Chrome> {
        let profile = tempfile::Builder::new().prefix("canon-chrome-").tempdir().context("cannot create a temporary profile directory")?;
        let mut cmd = Command::new(path);
        cmd.arg("--headless=new")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile.path().display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--disable-default-apps",
                "--mute-audio",
                "--hide-scrollbars",
                // Deterministic text: no LCD antialiasing, fixed colour profile and scale.
                "--disable-lcd-text",
                "--force-color-profile=srgb",
                "--force-device-scale-factor=1",
                "--disable-features=Translate,OptimizationHints,MediaRouter",
                "--disable-background-timer-throttling",
                "--disable-renderer-backgrounding",
                "--disable-backgrounding-occluded-windows",
            ]);
        if !sandbox {
            cmd.arg("--no-sandbox");
        }
        cmd.arg("about:blank").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = cmd.spawn().with_context(|| format!("cannot start {}", path.display()))?;
        let stderr = child.stderr.take().expect("stderr is piped");
        let (tx, rx) = mpsc::channel();
        let tail = Arc::new(Mutex::new(Vec::<String>::new()));
        let tail2 = tail.clone();
        std::thread::spawn(move || {
            let mut tx = Some(tx);
            // Keep draining so Chrome never blocks on a full pipe.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(rest) = line.strip_prefix("DevTools listening on ") {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(rest.trim().to_string());
                    }
                } else if tx.is_some() {
                    let mut t = tail2.lock().unwrap();
                    if t.len() < 12 {
                        t.push(line);
                    }
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(ws_url) => Ok(Chrome { child, ws_url, _profile: profile }),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let said = tail.lock().unwrap().join("\n  ");
                bail!("Chrome did not open a DevTools endpoint{}", if said.is_empty() { String::new() } else { format!(":\n  {said}") });
            }
        }
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
