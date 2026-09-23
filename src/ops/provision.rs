//! Making a Debian container able to run the stack: docker from
//! get.docker.com, once. Idempotent, so it runs on every `new` and, when
//! docker is found missing, on `docker apply`.
//! There is no prebuilt docker template: this takes about two minutes and
//! happens a few times a year, and a template would be an artifact to keep
//! fresh for those minutes.

use anyhow::{bail, Result};

use crate::cmd;
use crate::pct;
use crate::stack;

const SCRIPT: &str = r#"set -e
export LC_ALL=C.UTF-8 LANG=C.UTF-8
if docker info >/dev/null 2>&1; then echo "docker: present"; exit 0; fi
if ! command -v apt-get >/dev/null 2>&1; then echo "not a Debian-family guest" >&2; exit 4; fi
echo "docker: installing from get.docker.com"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl ca-certificates >/dev/null
curl -fsSL https://get.docker.com | sh
systemctl enable --now docker >/dev/null 2>&1 || true
docker info >/dev/null 2>&1
echo "docker: smoke test"
docker run --rm hello-world >/dev/null
docker image rm hello-world >/dev/null 2>&1 || true
"#;

/// Installs docker in the guest if it is not there. Output on the terminal.
pub fn ensure_docker(vmid: u32) -> Result<()> {
    if stack::docker_ok(vmid)? {
        return Ok(());
    }
    match pct::exec_stream_within(vmid, SCRIPT, cmd::LONG_TIMEOUT) {
        Ok(()) => Ok(()),
        Err(e) => bail!("guest {vmid}: docker install failed: {e}"),
    }
}
