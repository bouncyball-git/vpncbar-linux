//! Profile model + profiles.json persistence.
//!
//! Schema mirrors the macOS app's `Profile` (Swift Codable) so the on-disk
//! `profiles.json` keeps the same field names. Secrets are NEVER stored here —
//! they live in the Secret Service (see `secrets.rs`), keyed by the profile uuid.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A single VPN profile. All optional fields: `None`/empty => directive omitted
/// (backend default). Required fields (name/gateway/id/username) are always present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// Stable identity; Secret Service keys off this, so renames are cosmetic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub name: String,    // display label
    pub gateway: String, // IPSec gateway / openconnect server
    pub id: String,      // IPSec ID (group name)
    pub username: String, // Xauth username (may be "DOMAIN\\user")

    // ---- vpnc options (IPSec) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authmode: Option<String>, // IKE Authmode: psk/cert/hybrid
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dh_group: Option<String>, // IKE DH Group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nat_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, rename = "ifmode", skip_serializing_if = "Option::is_none")]
    pub if_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_match_domains: Option<String>, // scoped-DNS match domains
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpd_timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_weak: Option<bool>, // defaults ON when None
    #[serde(default, rename = "singleDES", skip_serializing_if = "Option::is_none")]
    pub single_des: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_encryption: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weak_auth: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Vec<String>>, // verbatim vpnc.conf directives

    // ---- backend selection ----
    /// "vpnc" (default) | "openconnect".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    // ---- openconnect options (AnyConnect SSL) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_authgroup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_server_cert: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_otp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_protocol: Option<String>,
    #[serde(default, rename = "ocNoDTLS", skip_serializing_if = "Option::is_none")]
    pub oc_no_dtls: Option<bool>,
    #[serde(default, rename = "ocDPD", skip_serializing_if = "Option::is_none")]
    pub oc_dpd: Option<String>,
    #[serde(default, rename = "ocMTU", skip_serializing_if = "Option::is_none")]
    pub oc_mtu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_reconnect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oc_debug: Option<String>,
}

impl Profile {
    /// Backend of a profile: defaults to vpnc when unset (back-compat).
    pub fn is_openconnect(&self) -> bool {
        self.kind.as_deref().unwrap_or("vpnc") == "openconnect"
    }

    /// Stable id used for pidfiles/info/log/keychain; falls back to name.
    pub fn ident(&self) -> &str {
        self.uuid.as_deref().unwrap_or(&self.name)
    }

    /// Filesystem-safe form of the name (letters/digits/-/_ only), for filenames.
    pub fn safe_name(&self) -> String {
        self.name
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect()
    }
}

/// Trimmed non-empty value, or None — mirrors Swift's `ne()`.
pub fn ne(s: Option<&str>) -> Option<String> {
    let t = s?.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Split "DOMAIN\\user" into (domain, user); a plain "user" yields (None, "user").
pub fn split_domain_user(s: &str) -> (Option<String>, String) {
    match s.find('\\') {
        Some(i) => {
            let d = s[..i].trim();
            let u = s[i + 1..].trim();
            (
                if d.is_empty() { None } else { Some(d.to_string()) },
                u.to_string(),
            )
        }
        None => (None, s.to_string()),
    }
}

// ---- paths ----

/// ~/.config/vpncbar
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vpncbar")
}

pub fn profiles_path() -> PathBuf {
    config_dir().join("profiles.json")
}

/// User-writable, persistent runtime dir for pidfiles + per-session logs.
pub fn run_dir() -> PathBuf {
    config_dir().join("run")
}

/// Per-profile pidfile: "<uuid>-<name>.pid".
pub fn pid_file(p: &Profile) -> PathBuf {
    run_dir().join(format!("{}-{}.pid", p.ident(), p.safe_name()))
}

/// Per-profile session log: "<uuid>_<name>.log" (truncated per connect).
/// openconnect writes it live; for vpnc it's rebuilt from the boot log + journal.
pub fn log_file(p: &Profile) -> PathBuf {
    run_dir().join(format!("{}_{}.log", p.ident(), p.safe_name()))
}

/// vpnc's connect-phase capture: everything it printed to stdout/stderr before
/// detaching (the handshake — and at Debug ≥1 the full negotiation/hex dumps).
/// Frozen once the daemon backgrounds (it reopens its fds to /dev/null); the
/// user-facing session log is rebuilt from this + the journal's runtime lines.
pub fn boot_log_file(p: &Profile) -> PathBuf {
    run_dir().join(format!("{}_{}.boot.log", p.ident(), p.safe_name()))
}

/// Per-tunnel runtime info written by the network script on connect.
/// Lives in a fixed root-writable dir; transient (removed on disconnect).
pub fn info_file(p: &Profile) -> PathBuf {
    PathBuf::from("/run/vpncbar").join(format!("{}.info", p.ident()))
}

// ---- persistence ----

pub fn load_profiles() -> Vec<Profile> {
    let data = match std::fs::read(profiles_path()) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    serde_json::from_slice(&data).unwrap_or_default()
}

pub fn save_profiles(list: &[Profile]) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    let json = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
    std::fs::write(profiles_path(), json)
}

/// Insert or replace a profile by uuid (assigning one if absent), keeping the
/// list sorted by name. Mirrors the macOS `upsert` (minus secret writes, which
/// the caller does via `secrets`).
pub fn upsert(mut p: Profile) -> Profile {
    if p.uuid.is_none() {
        p.uuid = Some(uuid_v4());
    }
    let mut list: Vec<Profile> = load_profiles()
        .into_iter()
        .filter(|x| x.uuid != p.uuid)
        .collect();
    list.push(p.clone());
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let _ = save_profiles(&list);
    p
}

pub fn remove_profile(p: &Profile) {
    let list: Vec<Profile> = load_profiles()
        .into_iter()
        .filter(|x| x.uuid != p.uuid)
        .collect();
    let _ = save_profiles(&list);
}

/// Minimal RFC-4122 v4 UUID using OS randomness (avoids a uuid crate dep).
pub fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    getrandom(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelcase_schema_roundtrips() {
        // The on-disk schema must stay camelCase (matching the macOS app), with
        // the acronym fields kept verbatim. Regression guard for the authgroup bug.
        let json = r#"{
            "name":"p","gateway":"g","id":"i","username":"u",
            "dhGroup":"dh2","dnsMatchDomains":"a.com","ocAuthgroup":"E",
            "singleDES":true,"ocNoDTLS":true,"ocDPD":"30","ocMTU":"1300","ifmode":"tun"
        }"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.dh_group.as_deref(), Some("dh2"));
        assert_eq!(p.oc_authgroup.as_deref(), Some("E"));
        assert_eq!(p.single_des, Some(true));
        assert_eq!(p.oc_no_dtls, Some(true));
        assert_eq!(p.oc_dpd.as_deref(), Some("30"));
        assert_eq!(p.if_mode.as_deref(), Some("tun"));

        // Re-serializing must preserve those exact keys.
        let out = serde_json::to_string(&p).unwrap();
        for key in ["dhGroup", "ocAuthgroup", "singleDES", "ocNoDTLS", "ocDPD", "ocMTU", "ifmode"] {
            assert!(out.contains(key), "missing key {key} in {out}");
        }
    }

    #[test]
    fn domain_user_split() {
        assert_eq!(split_domain_user("ACME\\alice"), (Some("ACME".into()), "alice".into()));
        assert_eq!(split_domain_user("bob"), (None, "bob".into()));
    }

    #[test]
    fn ne_trims_and_nils() {
        assert_eq!(ne(Some("  x ")), Some("x".into()));
        assert_eq!(ne(Some("   ")), None);
        assert_eq!(ne(None), None);
    }
}

/// Fill `buf` with random bytes from /dev/urandom (falls back to a weak source).
fn getrandom(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // Fallback: time-seeded, only reached if /dev/urandom is unavailable.
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15);
    for byte in buf.iter_mut() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *byte = (seed & 0xff) as u8;
    }
}
