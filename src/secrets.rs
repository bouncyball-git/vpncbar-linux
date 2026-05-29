//! Secret storage via the freedesktop Secret Service, accessed through the
//! `secret-tool` CLI — the Linux analogue of the macOS app shelling out to
//! `/usr/bin/security`. Works with gnome-keyring, KWallet (ksecretd), etc.
//!
//! Items are keyed by a single `service` attribute equal to the macOS service
//! name ("vpnc-<uuid>-secret" / "...-password"), so the keying is uuid-stable
//! and renaming a profile never moves a secret.

use crate::model::Profile;
use crate::sys::{run, SECRET_TOOL};

/// Secret Service "service" attribute for a profile's secret/password, keyed off
/// the stable uuid (falling back to name) — mirrors macOS `kcService`.
pub fn kc_service(p: &Profile, kind: &str) -> String {
    format!("vpnc-{}-{}", p.ident(), kind)
}

/// Look up a stored secret by service attribute. None if absent/empty.
pub fn get(service: &str) -> Option<String> {
    let r = run(SECRET_TOOL, &["lookup", "service", service], None);
    if !r.ok() {
        return None;
    }
    // secret-tool prints the secret with no trailing newline.
    let v = r.out.trim_end_matches('\n').to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// Store (or update) a secret. The value is fed on stdin so it never appears in
/// argv. `account` is recorded as an extra attribute for readability.
pub fn store(service: &str, account: &str, value: &str) -> bool {
    run(
        SECRET_TOOL,
        &[
            "store",
            "--label",
            service,
            "service",
            service,
            "account",
            account,
        ],
        Some(value),
    )
    .ok()
}

/// Remove a stored secret (no-op if absent).
pub fn delete(service: &str) {
    let _ = run(SECRET_TOOL, &["clear", "service", service], None);
}
