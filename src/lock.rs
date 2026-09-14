//! One lock per guest on the node, so the daemon and a hand-run `apply` on
//! the same vmid take turns. `pct` has its own config lock for the wrapper;
//! this one covers the whole reconcile, including what runs inside the guest.

use std::fs::{self, File};
use std::os::unix::io::AsRawFd;

use anyhow::{bail, Context, Result};

pub struct GuestLock {
    _file: File,
}

const DIR: &str = "/run/lock/pve-compose";

impl GuestLock {
    /// Takes the lock, waiting up to `wait_secs` for another holder.
    pub fn take(vmid: u32, wait_secs: u32) -> Result<Self> {
        fs::create_dir_all(DIR).with_context(|| format!("cannot create {DIR}"))?;
        let path = format!("{DIR}/{vmid}.lock");
        let file = File::create(&path).with_context(|| format!("cannot open {path}"))?;
        let fd = file.as_raw_fd();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs as u64);
        loop {
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(GuestLock { _file: file });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(err).with_context(|| format!("flock {path}"));
            }
            if std::time::Instant::now() >= deadline {
                bail!("guest {vmid} is busy (another pve-compose holds {path})");
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}
