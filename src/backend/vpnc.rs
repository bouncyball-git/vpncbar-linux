//! vpnc (Cisco IPSec, IKEv1 + XAUTH) backend.
//!
//! Differences from the macOS app: we use the distro's `/usr/bin/vpnc` (not a
//! vendored static build) and escalate via polkit. Stock vpnc has no `--log-file`
//! (that was a vendored patch), so we capture the handshake output ourselves and
//! write it to the per-profile log for the Debug tab.

use super::{script_invocation, ActionResult};
use crate::model::{boot_log_file, ne, pid_file, split_domain_user, Profile};
use crate::privilege::{self, was_dismissed};
use crate::secrets;
use crate::sys::{resolve_gateway_ip, VPNC};

/// Build the vpnc.conf text fed to vpnc on stdin (with secrets inlined). Secrets
/// are read from the Secret Service; never written to disk.
pub fn build_config(p: &Profile) -> Result<String, ActionResult> {
    let authmode = ne(p.authmode.as_deref()).unwrap_or_else(|| "psk".into());
    let uses_cert = authmode == "cert" || authmode == "hybrid";

    // Username may be "DOMAIN\\user": domain via vpnc's Domain directive. Fall
    // back to the standalone `domain` field for legacy profiles that stored it
    // separately.
    let (xauth_domain, xauth_user) = split_domain_user(&p.username);
    let xauth_domain = xauth_domain.or_else(|| ne(p.domain.as_deref()));

    let mut lines = vec![
        format!("IPSec gateway {}", resolve_gateway_ip(&p.gateway)),
        format!("IPSec ID {}", p.id),
        format!("IKE Authmode {authmode}"),
        format!("Xauth username {xauth_user}"),
    ];

    if uses_cert {
        let Some(ca) = ne(p.ca_file.as_deref()) else {
            return Err(ActionResult::Message(format!(
                "{authmode} auth needs a CA file.\nOpen the profile editor and set it."
            )));
        };
        lines.push(format!("CA-File {ca}"));
    } else {
        let Some(secret) = secrets::get(&secrets::kc_service(p, "secret")) else {
            return Err(ActionResult::Message(format!(
                "Group secret not found for \u{201c}{}\u{201d}.\nOpen the profile editor and set it.",
                p.name
            )));
        };
        lines.push(format!("IPSec secret {secret}"));
    }

    let password = secrets::get(&secrets::kc_service(p, "password"));
    if let Some(d) = xauth_domain {
        lines.push(format!("Domain {d}"));
    }
    if let Some(pw) = &password {
        lines.push(format!("Xauth password {pw}"));
    }

    let mut add = |key: &str, value: &Option<String>| {
        if let Some(v) = ne(value.as_deref()) {
            lines.push(format!("{key} {v}"));
        }
    };
    add("IKE DH Group", &p.dh_group);
    add("Perfect Forward Secrecy", &p.pfs);
    add("NAT Traversal Mode", &p.nat_mode);
    add("Vendor", &p.vendor);
    add("Interface MTU", &p.mtu);
    add("DPD idle timeout (our side)", &p.dpd_timeout);
    add("Debug", &p.debug);

    // Boolean directives (no value). Weak encryption defaults ON.
    if p.enable_weak.unwrap_or(true) {
        lines.push("Enable weak encryption".into());
    }
    if p.single_des.unwrap_or(false) {
        lines.push("Enable Single DES".into());
    }
    if p.no_encryption.unwrap_or(false) {
        lines.push("Enable no encryption".into());
    }
    if p.weak_auth.unwrap_or(false) {
        lines.push("Enable weak authentication".into());
    }

    lines.push(format!("Script {}", script_invocation(p)));
    if let Some(extra) = &p.extra {
        lines.extend(extra.iter().cloned());
    }
    Ok(lines.join("\n") + "\n")
}

pub fn connect(p: &Profile) -> ActionResult {
    let config = match build_config(p) {
        Ok(c) => c,
        Err(e) => return e,
    };

    let log = boot_log_file(p);
    let pid = pid_file(p);
    let pid_s = pid.to_string_lossy();
    // vpnc reads its config from stdin ("-"), detaches after the tunnel is up,
    // and writes its pid to --pid-file. We escalate the whole thing via pkexec.
    // Stock vpnc has no --log-file (that was a vendored patch) and reopens its
    // std fds to /dev/null when it daemonises, so redirecting stdout/stderr to
    // the BOOT log captures the full connection phase (handshake + debug +
    // errors) — frozen once it detaches. The runtime goes to syslog; the
    // user-facing session log is rebuilt from both by `session_log`.
    // (openconnect, which keeps stderr under --background, writes its whole
    // session straight to the session log instead.)
    let r = privilege::run_root_to_file(
        VPNC,
        &["--non-inter", "--pid-file", &pid_s, "-"],
        Some(&config),
        &log,
    );

    if r.ok() {
        return ActionResult::Ok;
    }
    if was_dismissed(&r) {
        return ActionResult::Message("Authorization was cancelled.".into());
    }
    // `r.out` holds the log text captured before the foreground process exited.
    let detail = crate::sys::tail_chars(&r.out, 600);
    let detail = if detail.is_empty() { r.out.clone() } else { detail };
    ActionResult::Message(format!("vpnc failed (status {}):\n{detail}", r.status))
}

/// This tunnel's runtime lines from the journal (vpnc daemonises to syslog),
/// scoped to the live PID so concurrent tunnels don't mix. `since` (unix
/// seconds) filters to newer lines — that's how "Clear log" works, since the
/// system journal itself can't be truncated. `Err(())` if journalctl failed
/// (e.g. the user can't read the system journal).
fn journal_log(pid: u32, since: Option<u64>) -> Result<String, ()> {
    let pid_arg = format!("_PID={pid}");
    let mut args = vec!["-t", "vpnc", &pid_arg, "-o", "cat", "--no-pager", "-n", "2000"];
    let since_arg;
    if let Some(s) = since {
        since_arg = format!("--since=@{s}");
        args.push(&since_arg);
    }
    let r = crate::sys::run("/usr/bin/journalctl", &args, None);
    if r.ok() {
        Ok(r.out)
    } else {
        Err(())
    }
}

/// Rebuild the user-facing session text for a live vpnc tunnel: the connect-phase
/// boot log (handshake/debug captured on stdout/stderr before vpnc detached)
/// followed by the runtime lines vpnc sent to syslog. Callers persist this to
/// `log_file(p)` so the Debug tab and "Reveal log" work off a real file, like
/// openconnect's.
pub fn session_log(p: &Profile, pid: u32, since: Option<u64>) -> Result<String, ()> {
    let journal = journal_log(pid, since)?;
    let boot = std::fs::read_to_string(boot_log_file(p)).unwrap_or_default();
    let mut out = String::new();
    if !boot.trim().is_empty() {
        out.push_str(boot.trim_end());
        out.push('\n');
    }
    out.push_str(&journal);
    Ok(out)
}
