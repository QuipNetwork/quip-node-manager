// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Wrapper around std::process::Command that suppresses console windows
// on Windows. All subprocess spawning should use `cmd::new()` instead
// of `Command::new()` directly.

use std::process::Command;

/// Create a `Command` with `CREATE_NO_WINDOW` set on Windows so that
/// subprocess invocations (docker, nvidia-smi, etc.) don't flash a
/// visible console window.
pub fn new(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
