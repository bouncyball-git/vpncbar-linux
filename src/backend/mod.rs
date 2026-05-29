//! Backend orchestration: connect/disconnect for vpnc (IPSec) and openconnect
//! (AnyConnect SSL). Mirrors the macOS app's connect/disconnect logic, adapted
//! to Linux (polkit escalation, system binaries, procfs state).

pub mod openconnect;
pub mod vpnc;

use crate::model::{ne, pid_file, Profile};
use crate::privilege::{self, was_dismissed};
use crate::sys::{disconnect_helper, vpnc_script, KILL, PKEXEC, VPNC};
use crate::tunnel::connected_tunnels;

/// The escalated command line VpncBar runs (Info tab). Secrets are piped on
/// stdin (vpnc's trailing "-" / openconnect's --passwd-on-stdin), so they're
/// not in the argv shown here.
pub fn command_line(p: &Profile) -> String {
    if p.is_openconnect() {
        format!("{PKEXEC} {}", openconnect::build_args(p).join(" "))
    } else {
        format!(
            "{PKEXEC} {VPNC} --non-inter --pid-file {} -",
            pid_file(p).display()
        )
    }
}

/// Outcome of a connect/disconnect action. `Message` carries a user-facing error.
#[derive(Debug)]
pub enum ActionResult {
    Ok,
    Message(String),
}

/// "VPNPID='…' [VPNC_MATCH_DOMAINS='…']" — env prefix for our network script,
/// passed as a shell prefix on the Script/--script value (run via /bin/sh).
/// VPNPID pins the per-tunnel info file; VPNC_MATCH_DOMAINS drives scoped DNS.
pub fn script_env_prefix(p: &Profile) -> String {
    let mut s = format!("VPNPID='{}'", p.ident());
    if let Some(raw) = ne(p.dns_match_domains.as_deref()) {
        let domains: String = raw
            .chars()
            .map(|c| if c == ',' || c == ' ' { ' ' } else { c })
            .filter(|c| c.is_alphanumeric() || ". -_".contains(*c))
            .collect();
        let joined = domains.split_whitespace().collect::<Vec<_>>().join(" ");
        if !joined.is_empty() {
            s += &format!(" VPNC_MATCH_DOMAINS='{joined}'");
        }
    }
    s
}

/// The network script invocation with its env prefix (shared by both backends).
pub fn script_invocation(p: &Profile) -> String {
    format!("{} {}", script_env_prefix(p), vpnc_script())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_prefix_normalizes_domains() {
        let mut p = Profile {
            uuid: Some("abc".into()),
            dns_match_domains: Some("corp.local, example.com".into()),
            ..Default::default()
        };
        assert_eq!(
            script_env_prefix(&p),
            "VPNPID='abc' VPNC_MATCH_DOMAINS='corp.local example.com'"
        );
        p.dns_match_domains = None;
        assert_eq!(script_env_prefix(&p), "VPNPID='abc'");
    }
}

/// Connect a profile. No-op (Ok) if it's already up. `otp` is a one-time 2FA
/// code for openconnect profiles that need one.
pub fn connect(p: &Profile, otp: Option<&str>) -> ActionResult {
    // Never launch a second daemon for an already-connected profile.
    if !connected_tunnels(std::slice::from_ref(p)).is_empty() {
        return ActionResult::Ok;
    }
    if p.is_openconnect() {
        openconnect::connect(p, otp)
    } else {
        vpnc::connect(p)
    }
}

/// Disconnect a profile by sending SIGTERM to its daemon (which runs the script's
/// teardown, restoring routes/DNS). Prefers the live pid from procfs, falling
/// back to the pidfile. The kill needs root (the daemon is root-owned).
pub fn disconnect(p: &Profile) -> ActionResult {
    let pid = match connected_tunnels(std::slice::from_ref(p)).get(&p.name) {
        Some((pid, _)) => *pid,
        None => {
            // Not running (already down) — nothing to do.
            return ActionResult::Ok;
        }
    };
    let pid_s = pid.to_string();
    // Prefer the installed helper (verifies the target is vpnc/openconnect before
    // killing, and is covered by the passwordless polkit rule). Fall back to a
    // plain `kill` when running uninstalled.
    let r = match disconnect_helper() {
        Some(helper) => privilege::run_root(helper, &[&pid_s], None),
        None => privilege::run_root(KILL, &["-TERM", &pid_s], None),
    };
    if r.ok() {
        return ActionResult::Ok;
    }
    if was_dismissed(&r) {
        return ActionResult::Message("Authorization was cancelled.".into());
    }
    ActionResult::Message(format!(
        "disconnect failed (status {}):\n{}",
        r.status,
        if r.err.is_empty() { &r.out } else { &r.err }
    ))
}
