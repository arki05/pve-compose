//! systemd's readiness and watchdog protocol, hand-rolled: one datagram to
//! `$NOTIFY_SOCKET` is all `sd_notify(3)` is for a single-process daemon, and
//! a dependency for it would be larger than the code.
//!
//! Without that variable -- a `pve-compose daemon` run at a terminal -- every
//! call here does nothing, so the loop never has to ask whether it is under
//! systemd. Nothing is ever reported as an error: a daemon that cannot tell
//! systemd how it is doing still reconciles guests.

use std::os::unix::net::{SocketAddr, UnixDatagram};

/// `READY=1`: the loop is polling. The unit is `Type=notify`, so systemd
/// waits for this before it calls the service started.
pub fn ready() {
    send("READY=1\n");
}

/// `WATCHDOG=1`: sent around every poll and after every guest, so the unit's
/// `WatchdogSec` measures the loop making progress and not just the process
/// existing.
pub fn watchdog() {
    send("WATCHDOG=1\n");
}

fn send(message: &str) {
    let Ok(path) = std::env::var("NOTIFY_SOCKET") else {
        return;
    };
    let (Ok(socket), Some(addr)) = (UnixDatagram::unbound(), address(&path)) else {
        return;
    };
    let _ = socket.send_to_addr(message.as_bytes(), &addr);
}

/// systemd's own rule: a leading `@` means the abstract namespace, anything
/// else is a path.
#[cfg(target_os = "linux")]
fn address(path: &str) -> Option<SocketAddr> {
    use std::os::linux::net::SocketAddrExt as _;
    match path.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name.as_bytes()).ok(),
        None => SocketAddr::from_pathname(path).ok(),
    }
}

/// There is no systemd off Linux; this is here so the crate still builds on
/// a developer's machine.
#[cfg(not(target_os = "linux"))]
fn address(path: &str) -> Option<SocketAddr> {
    SocketAddr::from_pathname(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_a_ping_that_goes_nowhere() {
        assert!(address("/run/systemd/notify").is_some());
        assert!(address("/run/\0/notify").is_none());
        #[cfg(target_os = "linux")]
        assert!(address("@abstract-notify").is_some());
        // Whatever the environment is, a ping neither panics nor blocks.
        watchdog();
    }
}
