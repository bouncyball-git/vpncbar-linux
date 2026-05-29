//! Small process/shell helpers — the Linux analogue of the macOS app's `run()`.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::ToSocketAddrs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Tool paths. vpnc/openconnect are the distro packages (not vendored).
pub const VPNC: &str = "/usr/bin/vpnc";
pub const PKEXEC: &str = "/usr/bin/pkexec";
pub const SECRET_TOOL: &str = "/usr/bin/secret-tool";
pub const KILL: &str = "/usr/bin/kill";
pub const CISCO_DECRYPT: &str = "/usr/bin/cisco-decrypt";

/// Our installed network script (wraps the distro vpnc-script: writes the Info
/// file + folds in scoped-DNS match domains) and the validated disconnect helper.
pub const VPNCBAR_SCRIPT: &str = "/usr/lib/vpncbar/vpncbar-script";
pub const VPNCBAR_DISCONNECT: &str = "/usr/lib/vpncbar/vpncbar-disconnect";

/// Network-config script for vpnc/openconnect. Prefers our installed wrapper
/// (so the Info tab + scoped DNS work); falls back to the distro's script when
/// running uninstalled. Overridable via VPNCBAR_SCRIPT.
pub fn vpnc_script() -> String {
    if let Ok(s) = std::env::var("VPNCBAR_SCRIPT") {
        return s;
    }
    if std::path::Path::new(VPNCBAR_SCRIPT).exists() {
        VPNCBAR_SCRIPT.to_string()
    } else {
        "/etc/vpnc/vpnc-script".to_string()
    }
}

/// The installed disconnect helper, if present (covered by the polkit rule).
pub fn disconnect_helper() -> Option<&'static str> {
    std::path::Path::new(VPNCBAR_DISCONNECT)
        .exists()
        .then_some(VPNCBAR_DISCONNECT)
}

/// First existing openconnect binary (Homebrew-style search is pointless on Linux,
/// but keep a couple of fallbacks). None if not installed.
pub fn openconnect_path() -> Option<&'static str> {
    for p in ["/usr/bin/openconnect", "/usr/local/bin/openconnect"] {
        if std::path::Path::new(p).exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Debug)]
pub struct Output {
    pub status: i32,
    pub out: String,
    pub err: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

/// Run a program to completion, optionally feeding `stdin`, capturing stdout/stderr.
pub fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Output {
                status: -1,
                out: String::new(),
                err: format!("failed to launch {program}: {e}"),
            }
        }
    };
    if let Some(data) = stdin {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(data.as_bytes());
            // drop closes the pipe
        }
    }
    match child.wait_with_output() {
        Ok(o) => Output {
            status: o.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&o.stdout).into_owned(),
            err: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Output {
            status: -1,
            out: String::new(),
            err: e.to_string(),
        },
    }
}

/// Like `run`, but the child's stdout+stderr go to `log` (truncated first)
/// instead of pipes. Because the open file descriptor is inherited across the
/// daemon's fork, vpnc/openconnect keep writing the *live* session to it after
/// they background — the Linux stand-in for the macOS vendored `--log-file`,
/// so the Debug tab tails the whole session rather than just the handshake.
/// The captured text is read back into `out` (after the foreground process
/// exits) for error reporting.
pub fn run_to_file(program: &str, args: &[&str], stdin: Option<&str>, log: &Path) -> Output {
    let file = match OpenOptions::new().create(true).write(true).truncate(true).open(log) {
        Ok(f) => f,
        Err(e) => {
            return Output {
                status: -1,
                out: String::new(),
                err: format!("cannot open log {}: {e}", log.display()),
            }
        }
    };
    let (out_f, err_f) = match (file.try_clone(), file.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            return Output { status: -1, out: String::new(), err: "cannot dup log fd".into() }
        }
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::from(out_f))
        .stderr(Stdio::from(err_f))
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Output { status: -1, out: String::new(), err: format!("failed to launch {program}: {e}") }
        }
    };
    if let Some(data) = stdin {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(data.as_bytes());
        }
    }
    let status = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    // Read back what was written before the foreground process exited (the
    // handshake / any error) so the caller can surface it.
    let out = std::fs::read_to_string(log).unwrap_or_default();
    Output { status, out, err: String::new() }
}

/// Whether the installed vpnc was built with X.509 (cert/hybrid) support. Stock
/// builds are PSK + XAUTH only; cert auth needs GnuTLS. We look for it in the
/// binary's shared-library dependencies — the Linux analogue of the macOS
/// `otool -L` check. Cached.
pub fn vpnc_supports_certs() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let r = run("/usr/bin/ldd", &[VPNC], None);
        format!("{}{}", r.out, r.err).to_lowercase().contains("gnutls")
    })
}

/// First non-empty line of `<bin> --version` (stdout, falling back to stderr).
/// None if the binary is absent.
pub fn tool_version(bin: &str) -> Option<String> {
    if !Path::new(bin).exists() {
        return None;
    }
    let r = run(bin, &["--version"], None);
    let text = if r.out.trim().is_empty() { r.err } else { r.out };
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// Resolve a hostname to an IPv4 literal so the gateway never depends on DNS
/// (immune to having its own domain scoped to the VPN's internal resolver).
/// Caches the last good lookup, mirroring the macOS `resolveGatewayIP`.
pub fn resolve_gateway_ip(host: &str) -> String {
    // Already an IPv4 literal?
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return host.to_string();
    }
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    if let Ok(addrs) = (host, 0u16).to_socket_addrs() {
        for a in addrs {
            if let std::net::IpAddr::V4(v4) = a.ip() {
                let ip = v4.to_string();
                cache.lock().unwrap().insert(host.to_string(), ip.clone());
                return ip;
            }
        }
    }
    // Fall back to last good IP, else the hostname.
    cache
        .lock()
        .unwrap()
        .get(host)
        .cloned()
        .unwrap_or_else(|| host.to_string())
}

/// Last `n` characters of a string (for surfacing the tail of a log/error).
pub fn tail_chars(s: &str, n: usize) -> String {
    let s = s.trim();
    let count = s.chars().count();
    if count > n {
        s.chars().skip(count - n).collect()
    } else {
        s.to_string()
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
