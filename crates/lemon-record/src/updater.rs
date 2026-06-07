//! In-app auto-update via `cargo-packager-updater`.
//!
//! The updater verifies a minisign signature against the embedded public key,
//! then (on macOS) swaps the running `.app` bundle in place. There is no
//! built-in relaunch on macOS, so we `open -n` the bundle and exit ourselves.
//!
//! All blocking work (HTTP check, download, install) runs on a long-lived
//! worker thread. The UI only ever reads a shared `UpdateState` — the egui
//! loop already repaints continuously, so it reflects progress without any
//! explicit wake-up. The `Update` object stays on the worker thread (it never
//! needs to be `Send`): the worker keeps the one it found between a Check and a
//! subsequent Install.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use cargo_packager_updater::{check_update, Config, Update};

/// Public minisign key (from `cargo packager signer generate`). Must match the
/// private key CI signs releases with, or the install is rejected.
const PUBKEY: &str = include_str!("../updater.pub");

/// Where the updater manifest lives. GitHub serves the asset attached to the
/// most recent published (non-draft) release at this stable URL.
const ENDPOINT: &str =
    "https://github.com/chronick/lemon-record/releases/latest/download/latest.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    /// A newer version is available (carries its version string).
    Available(String),
    Downloading,
    /// Installed in place; awaiting relaunch.
    Ready(String),
    Error(String),
}

enum Cmd {
    Check,
    Install,
}

pub struct Updater {
    tx: Sender<Cmd>,
    state: Arc<Mutex<UpdateState>>,
    bundle: Option<PathBuf>,
}

impl Updater {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let state = Arc::new(Mutex::new(UpdateState::Idle));
        let worker_state = state.clone();
        std::thread::spawn(move || worker(rx, worker_state));
        Self {
            tx,
            state,
            bundle: bundle_path(),
        }
    }

    /// Current app version (compile-time, single-sourced from Cargo.toml).
    pub fn current_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// True only for a packaged `.app` build — a `cargo run` binary can't
    /// self-update (nothing to swap), so the UI hides the controls.
    pub fn is_bundled(&self) -> bool {
        self.bundle.is_some()
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }

    pub fn check(&self) {
        let _ = self.tx.send(Cmd::Check);
    }

    pub fn install(&self) {
        let _ = self.tx.send(Cmd::Install);
    }

    /// Relaunch the freshly-installed bundle and exit this process.
    pub fn relaunch_and_exit(&self) -> ! {
        if let Some(app) = &self.bundle {
            let _ = std::process::Command::new("open").arg("-n").arg(app).spawn();
        }
        std::process::exit(0);
    }
}

fn set(state: &Mutex<UpdateState>, s: UpdateState) {
    *state.lock().unwrap() = s;
}

fn worker(rx: Receiver<Cmd>, state: Arc<Mutex<UpdateState>>) {
    // The update found by the last successful Check, kept here for Install.
    let mut pending: Option<Update> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Check => {
                set(&state, UpdateState::Checking);
                match check_now() {
                    Ok(Some(update)) => {
                        let v = update.version.clone();
                        pending = Some(update);
                        set(&state, UpdateState::Available(v));
                    }
                    Ok(None) => {
                        pending = None;
                        set(&state, UpdateState::UpToDate);
                    }
                    Err(e) => set(&state, UpdateState::Error(e)),
                }
            }
            Cmd::Install => {
                if let Some(update) = pending.take() {
                    let v = update.version.clone();
                    set(&state, UpdateState::Downloading);
                    match update.download_and_install() {
                        Ok(()) => set(&state, UpdateState::Ready(v)),
                        Err(e) => set(&state, UpdateState::Error(e.to_string())),
                    }
                }
            }
        }
    }
}

fn check_now() -> Result<Option<Update>, String> {
    let endpoint = ENDPOINT.parse().map_err(|e| format!("bad endpoint url: {e}"))?;
    let config = Config {
        endpoints: vec![endpoint],
        pubkey: PUBKEY.to_string(),
        ..Default::default()
    };
    let current = Updater::current_version()
        .parse()
        .map_err(|e| format!("bad current version: {e}"))?;
    check_update(current, config).map_err(|e| e.to_string())
}

/// Resolve the enclosing `.app` from the running executable
/// (`…/LEMON record.app/Contents/MacOS/lemon-record` → `…/LEMON record.app`).
/// Returns `None` for a bare `cargo run` binary.
fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    if app.extension().is_some_and(|e| e == "app") {
        Some(app.to_path_buf())
    } else {
        None
    }
}
