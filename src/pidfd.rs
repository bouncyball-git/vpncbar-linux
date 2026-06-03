//! Minimal `pidfd` watcher.
//!
//! Opening a PID file descriptor lets the glib main loop wake us the moment a
//! tunnel's `vpnc`/`openconnect` process exits, so we detect drops promptly
//! WITHOUT periodically scanning `/proc`. Verified on this target (kernel 6.18):
//! `pidfd_open(2)` succeeds for a non-child, root-owned process from an
//! unprivileged caller, and `poll(2)` reports that process's exit. The daemon
//! runs as root under pkexec and is reparented away from us, so both properties
//! matter. (`pidfd_open` is Linux 5.3+; we target current kernels.)

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// Open a `pidfd` for `pid`. Returns `None` if the process is already gone or
/// the syscall is unavailable — callers fall back to the periodic scan.
pub fn open(pid: u32) -> Option<OwnedFd> {
    // SAFETY: thin syscall wrapper; flags = 0. Returns a new owned fd or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a freshly returned file descriptor we now own.
    Some(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}
