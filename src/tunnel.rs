//! Live tunnel state, read from procfs (the Linux analogue of the macOS app's
//! `ps`/`netstat` parsing). One vpnc/openconnect process per profile, identified
//! by its `--pid-file <…/<uuid>-<name>.pid>` argument.

use crate::model::{info_file, pid_file, Profile};
use std::collections::HashMap;

const CLK_TCK: f64 = 100.0; // _SC_CLK_TCK is 100 on all mainstream Linux.

/// Connected profiles -> (live pid, elapsed seconds), keyed by profile name.
/// Scans /proc for vpnc/openconnect processes carrying `--pid-file`, maps each
/// to a profile by the pidfile stem (uuid prefix, else legacy name). Falls back
/// to the pidfile for any profile not seen in the process list.
pub fn connected_tunnels(profiles: &[Profile]) -> HashMap<String, (u32, u64)> {
    let uptime = uptime_secs();
    let mut result: HashMap<String, (u32, u64)> = HashMap::new();

    let match_stem = |stem: &str| -> Option<&Profile> {
        profiles
            .iter()
            .find(|p| p.uuid.as_deref().map(|u| stem == format!("{u}-{}", p.safe_name())).unwrap_or(false))
            .or_else(|| profiles.iter().find(|p| p.name == stem))
    };

    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let pid: u32 = match e.file_name().to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let argv = match read_cmdline(pid) {
                Some(a) if !a.is_empty() => a,
                _ => continue,
            };
            if !argv.iter().any(|a| is_backend_bin(a)) {
                continue;
            }
            // Pidfile stem: basename of the --pid-file value, minus ".pid".
            let Some(i) = argv.iter().position(|a| a == "--pid-file") else { continue };
            let Some(path) = argv.get(i + 1) else { continue };
            let base = path.rsplit('/').next().unwrap_or(path);
            let Some(stem) = base.strip_suffix(".pid") else { continue };
            let Some(p) = match_stem(stem) else { continue };
            if let Some(secs) = elapsed_for(pid, uptime) {
                result.insert(p.name.clone(), (pid, secs));
            }
        }
    }

    // Fallback: pidfile for profiles not matched above.
    for p in profiles {
        if result.contains_key(&p.name) {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(pid_file(p)) {
            if let Ok(pid) = s.trim().parse::<u32>() {
                if read_cmdline(pid).map(|a| a.iter().any(|x| is_backend_bin(x))).unwrap_or(false) {
                    if let Some(secs) = elapsed_for(pid, uptime) {
                        result.insert(p.name.clone(), (pid, secs));
                    }
                }
            }
        }
    }
    result
}

/// Whether a profile currently has a live tunnel.
pub fn is_connected(p: &Profile) -> bool {
    !connected_tunnels(std::slice::from_ref(p)).is_empty()
}

/// True if `arg` names the vpnc or openconnect binary.
fn is_backend_bin(arg: &str) -> bool {
    let base = arg.rsplit('/').next().unwrap_or(arg);
    base == "vpnc" || base == "openconnect"
}

fn read_cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}

fn uptime_secs() -> f64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

/// Elapsed seconds for a pid: system uptime minus the process start time
/// (field 22 of /proc/<pid>/stat, in clock ticks). None if the pid is gone.
fn elapsed_for(pid: u32, uptime: f64) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field (2nd) is parenthesised and may contain spaces/parens;
    // everything after the last ')' is space-separated and parens-free.
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // field 3 -> index 0, so field 22 (starttime) -> index 19.
    let start_ticks: f64 = fields.get(19)?.parse().ok()?;
    let secs = uptime - start_ticks / CLK_TCK;
    Some(secs.max(0.0) as u64)
}

pub fn format_elapsed(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

// ---- per-interface byte/packet counters (Info tab) ----

#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_pkts: u64,
    pub tx_pkts: u64,
}

/// rx/tx byte+packet counters for an interface, from /proc/net/dev.
pub fn interface_counters(iface: &str) -> Option<Counters> {
    let data = std::fs::read_to_string("/proc/net/dev").ok()?;
    for line in data.lines() {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if name.trim() != iface {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        // Receive: bytes packets errs drop fifo frame compressed multicast (8),
        // then Transmit: bytes packets ...
        if f.len() >= 10 {
            return Some(Counters {
                rx_bytes: f[0],
                rx_pkts: f[1],
                tx_bytes: f[8],
                tx_pkts: f[9],
            });
        }
    }
    None
}

// ---- per-tunnel runtime info written by the network script ----

#[derive(Debug, Default, Clone)]
pub struct TunnelInfo {
    pub iface: Option<String>,
    pub internal_ip: Option<String>,
    pub dns: Option<String>,
    pub gateway: Option<String>,
    pub def_domain: Option<String>,
    pub split_dns: Option<String>,
    pub match_domains: Option<String>,
    pub routes: Vec<String>,
}

/// Parse the key=value info file the network script records on connect.
pub fn read_tunnel_info(p: &Profile) -> TunnelInfo {
    let mut t = TunnelInfo::default();
    let Ok(raw) = std::fs::read_to_string(info_file(p)) else { return t };
    let nz = |v: &str| (!v.is_empty()).then(|| v.to_string());
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "TUNDEV" => t.iface = nz(v),
            "INTERNAL_IP4_ADDRESS" => t.internal_ip = nz(v),
            "INTERNAL_IP4_DNS" => t.dns = nz(v),
            "VPNGATEWAY" => t.gateway = nz(v),
            "CISCO_DEF_DOMAIN" => t.def_domain = nz(v),
            "CISCO_SPLIT_DNS" => t.split_dns = nz(v),
            "VPNC_MATCH_DOMAINS" => t.match_domains = nz(v),
            "ROUTE" => {
                if !v.is_empty() {
                    t.routes.push(v.to_string())
                }
            }
            _ => {}
        }
    }
    t
}
