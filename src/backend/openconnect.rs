//! openconnect (Cisco AnyConnect SSL and compatible) backend.
//!
//! Uses the system `/usr/bin/openconnect` and the same network script as vpnc,
//! so routes and scoped DNS behave identically. Escalated via polkit.

use super::{script_invocation, ActionResult};
use crate::model::{ne, log_file, pid_file, split_domain_user, Profile};
use crate::privilege::{self, was_dismissed};
use crate::secrets;
use crate::sys::{openconnect_path, run, tail_chars};

/// Build openconnect's argv (without the password, which goes on stdin). Shared
/// by connect and the Info-tab command display. Server is the gateway field.
pub fn build_args(p: &Profile) -> Vec<String> {
    let mut args = vec![
        openconnect_path().unwrap_or("openconnect").to_string(),
        "--background".into(),
        "--pid-file".into(),
        pid_file(p).to_string_lossy().into_owned(),
        "--script".into(),
        script_invocation(p), // VPNPID → Info tab + scoped DNS
        format!("--protocol={}", ne(p.oc_protocol.as_deref()).unwrap_or_else(|| "anyconnect".into())),
        "--passwd-on-stdin".into(),
        format!("--user={}", split_domain_user(&p.username).1),
    ];
    // Verbosity: 0 none · 1 -v · 2 -vv · 3 -vvv · 99 -vvv + full HTTP dump.
    match ne(p.oc_debug.as_deref()).as_deref().unwrap_or("1") {
        "1" => args.push("-v".into()),
        "2" => args.push("-vv".into()),
        "3" => args.push("-vvv".into()),
        "99" => {
            args.push("-vvv".into());
            args.push("--dump-http-traffic".into());
        }
        _ => {} // "0": no extra verbosity
    }
    if p.oc_no_dtls.unwrap_or(false) {
        args.push("--no-dtls".into());
    }
    if let Some(dpd) = ne(p.oc_dpd.as_deref()) {
        args.push("--dpd".into());
        args.push(dpd);
    }
    if let Some(mtu) = ne(p.oc_mtu.as_deref()) {
        args.push("--mtu".into());
        args.push(mtu);
    }
    if let Some(rc) = ne(p.oc_reconnect.as_deref()) {
        args.push("--reconnect-timeout".into());
        args.push(rc);
    }
    if let Some(g) = ne(p.oc_authgroup.as_deref()) {
        args.push(format!("--authgroup={g}"));
    }
    if let Some(pin) = ne(p.oc_server_cert.as_deref()) {
        args.push(format!("--servercert={pin}"));
    }
    if let Some(cert) = ne(p.client_cert.as_deref()) {
        args.push(format!("--certificate={cert}"));
    }
    args.push(p.gateway.clone()); // server (URL or host)
    args
}

pub fn connect(p: &Profile, otp: Option<&str>) -> ActionResult {
    if openconnect_path().is_none() {
        return ActionResult::Message(
            "openconnect isn't installed.\nInstall it with your package manager (e.g. pacman -S openconnect)."
                .into(),
        );
    }
    let password = secrets::get(&secrets::kc_service(p, "password")).unwrap_or_default();
    let mut input = format!("{password}\n");
    if let Some(otp) = ne(otp) {
        input += &format!("{otp}\n");
    }

    let log = log_file(p);

    // openconnect reads one value per form prompt from stdin: password, then
    // (for 2FA groups) the one-time code. It backgrounds after authenticating;
    // redirecting stdout/stderr to the log (the fd is inherited across the
    // --background fork) gives the Debug tab the whole live session.
    let args = build_args(p);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = privilege::run_root_to_file(argv[0], &argv[1..], Some(&input), &log);

    if r.ok() {
        return ActionResult::Ok;
    }
    if was_dismissed(&r) {
        return ActionResult::Message("Authorization was cancelled.".into());
    }
    ActionResult::Message(format!(
        "openconnect failed (status {}):\n{}",
        r.status,
        tail_chars(&r.out, 600)
    ))
}

/// Outcome of a group probe: the groups (empty on failure) plus, when the gateway
/// presented an untrusted cert and no pin was supplied, the pin-sha256 fingerprint
/// openconnect reported — so the caller can offer trust-on-first-use pinning.
pub struct GroupProbe {
    pub groups: Vec<(String, bool)>,
    pub cert_pin: Option<String>,
}

/// Fetch the gateway's group list AND each group's 2FA flag in ONE probe. The
/// 2FA requirement is encoded as second-auth="1" on the group's <option>;
/// openconnect's --authgroup matches the option's LABEL, so that's what we
/// store/use. No credentials needed (no tunnel, no root). Empty on failure.
pub fn group_list(server: &str, server_cert: Option<&str>) -> GroupProbe {
    let empty = || GroupProbe { groups: vec![], cert_pin: None };
    let Some(oc) = openconnect_path() else { return empty() };
    if server.is_empty() {
        return empty();
    }
    let mut args: Vec<String> = vec![
        "--protocol=anyconnect".into(),
        "--cookieonly".into(),
        "--dump-http-traffic".into(),
        "--user=probe".into(),
        "--passwd-on-stdin".into(),
    ];
    if let Some(pin) = ne(server_cert) {
        args.push(format!("--servercert={pin}"));
    }
    args.push(server.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    // Dummy stdin so openconnect reads past its prompts and dumps the form
    // (with the group list) before auth harmlessly fails.
    let r = run(oc, &argv, Some("x\ny\n"));
    let out = format!("{}\n{}", r.out, r.err);

    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Minimal <option …>label</option> scan (no regex dependency).
    parse_options(&out, &mut |attrs, label| {
        if !label.is_empty() && seen.insert(label.to_string()) {
            result.push((label.to_string(), attrs.to_lowercase().contains("second-auth=\"1\"")));
        }
    });

    // No groups + no pin supplied: if the gateway's cert wasn't trusted,
    // openconnect prints the pin-sha256 fingerprint. Surface it for TOFU pinning.
    let cert_pin = if result.is_empty() && ne(server_cert).is_none() {
        extract_cert_pin(&out)
    } else {
        None
    };
    GroupProbe { groups: result, cert_pin }
}

/// First `pin-sha256:<base64>` fingerprint in openconnect's output (the value it
/// suggests for --servercert when it rejects an untrusted gateway cert), or None.
fn extract_cert_pin(text: &str) -> Option<String> {
    const TAG: &str = "pin-sha256:";
    let start = text.find(TAG)?;
    let rest = &text[start + TAG.len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
        .unwrap_or(rest.len());
    let b64 = &rest[..end];
    (!b64.is_empty()).then(|| format!("{TAG}{b64}"))
}

/// Invoke `f(attrs, label)` for each `<option ATTRS>LABEL</option>` in `text`.
fn parse_options(text: &str, f: &mut dyn FnMut(&str, &str)) {
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<option") {
        let tag_start = from + rel;
        let Some(rel_gt) = lower[tag_start..].find('>') else { break };
        let gt = tag_start + rel_gt;
        let attrs = &text[tag_start + "<option".len()..gt];
        let body_start = gt + 1;
        let Some(rel_close) = lower[body_start..].find("</option>") else {
            from = body_start;
            continue;
        };
        let close = body_start + rel_close;
        let label = text[body_start..close].trim();
        f(attrs, label);
        from = close + "</option>".len();
    }
}
