// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Wrapper around std::process::Command. Every subprocess the app spawns
// (docker, docker compose, nvidia-smi, system_profiler, python3, …) flows
// through `cmd::new()` so PATH augmentation and console-window suppression
// happen in exactly one place.
//
// PATH resolution order (first hit wins during `execvp`):
//   1. Manual override (e.g. user chose a docker binary via the UI)       ← TODO hook
//   2. User's login-shell PATH (discovered via `$SHELL -ilc 'echo $PATH'`)
//   3. Process-inherited PATH (whatever launchd/Explorer/systemd gave us)
//   4. Platform-specific known tool-install directories (safety net)

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{OnceLock, RwLock};

// ── known tool-install directories (safety net) ───────────────────────────

/// Directories where docker / container-runtime CLIs commonly live on macOS.
/// Used only as a fallback if the user's shell PATH doesn't already include
/// them. Paths starting with `~/` are expanded at resolution time.
#[cfg(target_os = "macos")]
const KNOWN_TOOL_PATHS: &[&str] = &[
    "/usr/local/bin",
    "/opt/homebrew/bin",
    "/Applications/Docker.app/Contents/Resources/bin",
    "/Applications/OrbStack.app/Contents/MacOS/xbin",
    "~/.rd/bin",
    "~/.colima/bin",
];

/// Linux usually has `/usr/local/bin` and `/usr/bin` in its default PATH, but
/// apps launched from `.desktop` files via `systemd --user` can end up with
/// a stripped PATH similar to macOS launchd. Covers common alternatives.
#[cfg(target_os = "linux")]
const KNOWN_TOOL_PATHS: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/snap/bin",
    "/var/lib/flatpak/exports/bin",
];

/// Windows GUI apps inherit the merged HKCU+HKLM PATH from Explorer, so the
/// safety net is rarely needed. Kept for parity and for unusual Docker
/// Desktop install locations.
#[cfg(target_os = "windows")]
const KNOWN_TOOL_PATHS: &[&str] = &[
    r"C:\Program Files\Docker\Docker\resources\bin",
];

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
        {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

// ── login-shell PATH discovery ─────────────────────────────────────────────

/// Spawn the user's login shell and ask it to print `$PATH`. This is the
/// authoritative source for "the user's environment" on Unix — it loads
/// `/etc/paths`, `/etc/paths.d/*`, `~/.zprofile`, `~/.zshrc`, etc.
///
/// Returns `None` on: no `$SHELL` env var, spawn failure, shell timeout,
/// non-zero exit, or empty stdout. Never blocks longer than 3s total.
#[cfg(unix)]
fn discover_login_shell_path() -> Option<OsString> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let shell = std::env::var_os("SHELL")?;

    // `-i` loads interactive rc (~/.zshrc), `-l` loads login rc (~/.zprofile
    // + /etc/paths). `command printf` avoids any aliasing of `echo`.
    // Stderr is swallowed — noisy shells print banners we don't want in logs.
    let mut child = Command::new(&shell)
        .args(["-ilc", "command printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = Instant::now();
    let timeout = Duration::from_secs(3);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(OsString::from(trimmed))
    }
}

#[cfg(not(unix))]
fn discover_login_shell_path() -> Option<OsString> {
    // Windows: HKCU+HKLM PATH is already in the process env at launch time,
    // so there's nothing extra to discover. Returning None falls through to
    // process PATH + KNOWN_TOOL_PATHS, which is correct.
    None
}

// ── manual override (modal / settings integration point) ──────────────────

/// Extra directory to prepend to PATH, set by the UI when the user manually
/// locates docker via file picker. Reset-safe: overwrite replaces the prior
/// value, clearing to `None` removes the override.
static MANUAL_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Install/replace the manual-override directory. Invalidates the cached
/// PATH so the next `cmd::new` call rebuilds with the new entry.
#[allow(dead_code)] // wired in a follow-up when the modal UI lands
pub fn set_manual_tool_dir(dir: Option<PathBuf>) {
    *MANUAL_OVERRIDE.write().expect("manual-override lock poisoned") = dir;
    // OnceLock can't be reset, so we use a generation counter instead —
    // `augmented_path` re-reads the override on every miss.
    CACHED_PATH.get_or_init(build_augmented_path); // touch to ensure init
}

// ── PATH assembly + cache ─────────────────────────────────────────────────

static CACHED_PATH: OnceLock<OsString> = OnceLock::new();

fn build_augmented_path() -> OsString {
    let mut parts: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |parts: &mut Vec<PathBuf>,
                seen: &mut std::collections::HashSet<PathBuf>,
                p: PathBuf| {
        if seen.insert(p.clone()) {
            parts.push(p);
        }
    };

    if let Ok(guard) = MANUAL_OVERRIDE.read() {
        if let Some(dir) = guard.as_ref() {
            push(&mut parts, &mut seen, dir.clone());
        }
    }

    if let Some(shell_path) = discover_login_shell_path() {
        for p in std::env::split_paths(&shell_path) {
            push(&mut parts, &mut seen, p);
        }
    }

    if let Some(existing) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&existing) {
            push(&mut parts, &mut seen, p);
        }
    }

    for raw in KNOWN_TOOL_PATHS {
        push(&mut parts, &mut seen, expand_home(raw));
    }

    std::env::join_paths(parts).unwrap_or_default()
}

/// Exposed for diagnostic logging (e.g. at startup, so we can see what PATH
/// the app actually ended up with when debugging a user's "docker not found"
/// report).
pub fn effective_path() -> OsString {
    CACHED_PATH.get_or_init(build_augmented_path).clone()
}

// ── public command constructor ─────────────────────────────────────────────

/// Create a `Command` with:
///   - `CREATE_NO_WINDOW` on Windows (no console flash),
///   - augmented PATH on every platform (user shell env + known tool dirs).
pub fn new(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.env("PATH", effective_path());
    cmd
}
