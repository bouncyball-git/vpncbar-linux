//! Privilege escalation via polkit's `pkexec`.
//!
//! vpnc/openconnect need root to create the tun device and set routes. On macOS
//! this was `sudo -n` backed by a sudoers rule; on Linux we use polkit, which
//! integrates with the desktop auth agent. A custom polkit action (allowing
//! cached/passwordless auth for these specific binaries) is installed in
//! milestone 4; until then pkexec prompts through the session's polkit agent.
//!
//! Note: pkexec passes stdin/stdout/stderr through to the target, so vpnc's
//! config-on-stdin and openconnect's --passwd-on-stdin work unchanged. pkexec
//! resets the environment, but we pass the script's env vars as a shell prefix
//! on the "Script"/"--script" value (run via /bin/sh), not via the environment.

use crate::sys::{run, Output, PKEXEC};

/// Run `program args…` as root via pkexec, optionally feeding `stdin`.
pub fn run_root(program: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(program);
    argv.extend_from_slice(args);
    run(PKEXEC, &argv, stdin)
}

/// True if the failure looks like the user dismissed/failed the polkit auth.
pub fn was_dismissed(o: &Output) -> bool {
    let s = o.err.to_lowercase();
    // pkexec exit codes: 126 = not authorized / dismissed, 127 = exec failed.
    o.status == 126 || s.contains("not authorized") || s.contains("dismissed")
}
