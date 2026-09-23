//! The wrapper as PVE sees it. Reads go through `pvesh` (JSON), writes
//! through `pct`, and anything inside the guest through `pct exec` and
//! `pct push`. Nothing here knows about compose.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::cmd;
use crate::size::Size;

/// One `mpN` (or `rootfs`) line of a container config, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// `mp0`, `mp1`, ... or `rootfs`.
    pub key: String,
    /// A volid (`local-lvm:vm-105-disk-1`) or a host path for a bind mount.
    pub volume: String,
    /// The mount path inside the guest; absent for `rootfs`.
    pub mp: Option<String>,
    pub size: Option<Size>,
    /// `backup=1`; PVE's default for a volume mountpoint is off, and for
    /// `rootfs` it is always on.
    pub backup: bool,
    /// Every other option, kept so a rewrite loses nothing.
    pub other: BTreeMap<String, String>,
}

impl Mount {
    pub fn is_bind(&self) -> bool {
        self.volume.starts_with('/')
    }

    pub fn storage(&self) -> Option<&str> {
        if self.is_bind() {
            None
        } else {
            self.volume.split(':').next()
        }
    }

    pub fn index(&self) -> Option<u32> {
        self.key.strip_prefix("mp").and_then(|n| n.parse().ok())
    }

    /// Parses `local-lvm:vm-105-disk-1,mp=/x,size=8G,backup=1`.
    pub fn parse(key: &str, line: &str) -> Result<Self> {
        let mut parts = line.split(',');
        let first = parts.next().unwrap_or("").trim();
        let mut m = Mount {
            key: key.to_string(),
            volume: String::new(),
            mp: None,
            size: None,
            backup: false,
            other: BTreeMap::new(),
        };
        // The volume may be spelled `volume=...` or bare, PVE accepts both.
        if let Some(v) = first.strip_prefix("volume=") {
            m.volume = v.to_string();
        } else {
            m.volume = first.to_string();
        }
        for p in parts {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| anyhow!("bad mountpoint option '{p}' in {key}"))?;
            match k {
                "mp" => m.mp = Some(v.to_string()),
                "size" => m.size = Some(v.parse().with_context(|| format!("{key}: size"))?),
                "backup" => m.backup = v == "1",
                "volume" => m.volume = v.to_string(),
                _ => {
                    m.other.insert(k.to_string(), v.to_string());
                }
            }
        }
        if m.volume.is_empty() {
            bail!("{key}: no volume");
        }
        Ok(m)
    }
}

impl fmt::Display for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.volume)?;
        if let Some(mp) = &self.mp {
            write!(f, ",mp={mp}")?;
        }
        if let Some(s) = self.size {
            write!(f, ",size={s}")?;
        }
        if self.backup {
            write!(f, ",backup=1")?;
        }
        for (k, v) in &self.other {
            write!(f, ",{k}={v}")?;
        }
        Ok(())
    }
}

/// A container's config, the parts this tool reads.
#[derive(Debug, Clone, Default)]
pub struct GuestConfig {
    pub vmid: u32,
    pub hostname: Option<String>,
    pub unprivileged: bool,
    pub features: BTreeMap<String, String>,
    pub tags: Vec<String>,
    pub rootfs: Option<Mount>,
    pub mounts: Vec<Mount>,
    pub is_template: bool,
}

impl GuestConfig {
    pub fn mount_at(&self, path: &str) -> Option<&Mount> {
        self.mounts.iter().find(|m| m.mp.as_deref() == Some(path))
    }

    /// The lowest `mpN` index not in use.
    pub fn free_mp_index(&self) -> u32 {
        let used: Vec<u32> = self.mounts.iter().filter_map(Mount::index).collect();
        (0..256u32).find(|i| !used.contains(i)).unwrap_or(256)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

/// Parses the JSON of `GET /nodes/{node}/lxc/{vmid}/config`.
pub fn parse_config(vmid: u32, v: &Value) -> Result<GuestConfig> {
    let obj = v
        .as_object()
        .ok_or_else(|| anyhow!("config of {vmid} is not an object"))?;
    let mut c = GuestConfig {
        vmid,
        ..Default::default()
    };
    for (k, val) in obj {
        let s = || -> String {
            match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }
        };
        match k.as_str() {
            "hostname" => c.hostname = Some(s()),
            "unprivileged" => c.unprivileged = val.as_u64() == Some(1) || val.as_str() == Some("1"),
            "template" => c.is_template = val.as_u64() == Some(1) || val.as_str() == Some("1"),
            "tags" => {
                c.tags = s()
                    .split(';')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .collect()
            }
            "features" => {
                for item in s().split(',') {
                    let item = item.trim();
                    if item.is_empty() {
                        continue;
                    }
                    let (fk, fv) = item.split_once('=').unwrap_or((item, "1"));
                    c.features.insert(fk.to_string(), fv.to_string());
                }
            }
            "rootfs" => c.rootfs = Some(Mount::parse("rootfs", &s())?),
            _ if k.starts_with("mp") && k[2..].chars().all(|ch| ch.is_ascii_digit()) => {
                c.mounts.push(Mount::parse(k, &s())?);
            }
            _ => {}
        }
    }
    c.mounts.sort_by_key(|m| m.index());
    Ok(c)
}

/// Reads the config of a container on this node.
pub fn config(node: &str, vmid: u32) -> Result<GuestConfig> {
    let v = cmd::pvesh_get(&format!("/nodes/{node}/lxc/{vmid}/config"), &[])
        .with_context(|| format!("guest {vmid}: cannot read config"))?;
    parse_config(vmid, &v)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Stopped,
    Other,
}

pub fn status(node: &str, vmid: u32) -> Result<Status> {
    let v = cmd::pvesh_get(&format!("/nodes/{node}/lxc/{vmid}/status/current"), &[])
        .with_context(|| format!("guest {vmid}: cannot read status"))?;
    Ok(match v.get("status").and_then(Value::as_str) {
        Some("running") => Status::Running,
        Some("stopped") => Status::Stopped,
        _ => Status::Other,
    })
}

/// One row of `GET /nodes/{node}/lxc`.
#[derive(Debug, Clone)]
pub struct Listed {
    pub vmid: u32,
    pub name: String,
    pub running: bool,
    pub tags: Vec<String>,
    pub is_template: bool,
}

/// Every container on this node.
pub fn list(node: &str) -> Result<Vec<Listed>> {
    let v = cmd::pvesh_get(&format!("/nodes/{node}/lxc"), &[])?;
    let rows = v
        .as_array()
        .ok_or_else(|| anyhow!("lxc list is not an array"))?;
    let mut out = Vec::new();
    for r in rows {
        let vmid = match r.get("vmid") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            _ => 0,
        };
        if vmid == 0 {
            continue;
        }
        out.push(Listed {
            vmid,
            name: r
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            running: r.get("status").and_then(Value::as_str) == Some("running"),
            tags: r
                .get("tags")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split(';')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect(),
            is_template: r.get("template").and_then(Value::as_u64) == Some(1),
        });
    }
    out.sort_by_key(|l| l.vmid);
    Ok(out)
}

/// `pct set <vmid> <args>`. Allocating a disk is one of these, so it gets
/// the long deadline.
pub fn set(vmid: u32, args: &[&str]) -> Result<()> {
    let vm = vmid.to_string();
    let mut all = vec!["set", vm.as_str()];
    all.extend_from_slice(args);
    cmd::run_within("pct", &all, cmd::LONG_TIMEOUT).map(|_| ())
}

/// `pct resize <vmid> <disk> <size>`.
pub fn resize(vmid: u32, disk: &str, size: Size) -> Result<()> {
    let vm = vmid.to_string();
    cmd::run_within(
        "pct",
        &["resize", &vm, disk, &size.to_pct_resize()],
        cmd::LONG_TIMEOUT,
    )
    .map(|_| ())
}

/// The first line of a script, shortened: what names the call in an error
/// about a guest that did not answer.
fn brief(script: &str) -> String {
    let line = script
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(59).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Runs a shell command line inside the guest, capturing output. A non-zero
/// exit is returned in `Output.status`, not as an error; a guest that does
/// not answer within the deadline is one, naming the guest and the call.
pub fn exec_status(vmid: u32, script: &str) -> Result<cmd::Output> {
    exec_status_within(vmid, script, cmd::TIMEOUT)
}

/// [`exec_status`] with a deadline of its own.
pub fn exec_status_within(vmid: u32, script: &str, timeout: Duration) -> Result<cmd::Output> {
    let vm = vmid.to_string();
    cmd::run_status_within(
        "pct",
        &["exec", &vm, "--", "/bin/sh", "-c", script],
        timeout,
    )
    .with_context(|| format!("guest {vmid}: `{}`", brief(script)))
}

/// Runs a shell command line inside the guest; fails on a non-zero exit.
pub fn exec(vmid: u32, script: &str) -> Result<cmd::Output> {
    let out = exec_status(vmid, script)?;
    if out.status != 0 {
        bail!(
            "in guest {vmid}: `{}` failed (exit {}): {}",
            script,
            out.status,
            out.stderr.trim()
        );
    }
    Ok(out)
}

/// Runs a shell command line inside the guest with the terminal attached and
/// no deadline: for what a human typed and watches (`docker compose logs -f`).
pub fn exec_stream(vmid: u32, script: &str) -> Result<()> {
    let vm = vmid.to_string();
    cmd::stream("pct", &["exec", &vm, "--", "/bin/sh", "-c", script])
        .with_context(|| format!("guest {vmid}: `{}`", brief(script)))
}

/// [`exec_stream`] with a deadline: for the long ones the daemon runs too.
pub fn exec_stream_within(vmid: u32, script: &str, timeout: Duration) -> Result<()> {
    let vm = vmid.to_string();
    cmd::stream_within(
        "pct",
        &["exec", &vm, "--", "/bin/sh", "-c", script],
        timeout,
    )
    .with_context(|| format!("guest {vmid}: `{}`", brief(script)))
}

/// Reads a file from inside the guest. `Ok(None)` when it does not exist.
pub fn read_file(vmid: u32, path: &str) -> Result<Option<String>> {
    let script = format!("if [ -e '{path}' ]; then cat '{path}'; else exit 3; fi");
    let out = exec_status(vmid, &script)?;
    match out.status {
        0 => Ok(Some(out.stdout)),
        3 => Ok(None),
        s => bail!(
            "in guest {vmid}: cannot read {path} (exit {s}): {}",
            out.stderr.trim()
        ),
    }
}

// File writes inside the guest go through the guest-files library
// (`stack::write_managed`): validated paths, atomic rename and manifest
// tracking, not a bare `pct push`.

pub fn start(vmid: u32) -> Result<()> {
    cmd::run_within("pct", &["start", &vmid.to_string()], cmd::LONG_TIMEOUT).map(|_| ())
}

/// One storage of this node, from `GET /nodes/{node}/storage`.
#[derive(Debug, Clone)]
pub struct Storage {
    pub id: String,
    pub kind: String,
    pub avail: u64,
    pub shared: bool,
}

/// The enabled, active storages on `node` that accept `content`
/// (`rootdir` for container disks, `vztmpl` for templates).
pub fn storages(node: &str, content: &str) -> Result<Vec<Storage>> {
    let v = cmd::pvesh_get(
        &format!("/nodes/{node}/storage"),
        &["--content", content, "--enabled", "1"],
    )?;
    let mut out = Vec::new();
    for r in v.as_array().map(|a| a.iter()).into_iter().flatten() {
        if r.get("active").and_then(Value::as_u64) == Some(0) {
            continue;
        }
        let Some(id) = r.get("storage").and_then(Value::as_str) else {
            continue;
        };
        out.push(Storage {
            id: id.to_string(),
            kind: r
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            avail: r.get("avail").and_then(Value::as_u64).unwrap_or(0),
            shared: r.get("shared").and_then(Value::as_u64) == Some(1),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// The node owning `vmid`, from the cluster's own vmid map. `Ok(None)` for a
/// vmid that is not a guest anywhere.
pub fn owner_node(vmid: u32) -> Result<Option<(String, String)>> {
    let text = std::fs::read_to_string("/etc/pve/.vmlist").context("reading /etc/pve/.vmlist")?;
    let v: Value = serde_json::from_str(&text).context("/etc/pve/.vmlist is not JSON")?;
    let Some(entry) = v.get("ids").and_then(|ids| ids.get(vmid.to_string())) else {
        return Ok(None);
    };
    let node = entry
        .get("node")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(Some((node, kind)))
}

/// Waits until the guest answers `pct exec` and has a default route, for up
/// to `secs` seconds: the signal that apt and docker pulls can run. The
/// address itself is nobody's business here.
pub fn wait_ready(vmid: u32, secs: u32) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs as u64);
    loop {
        if let Ok(out) = exec_status(vmid, "ip route show default 2>/dev/null | grep -q .") {
            if out.status == 0 {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!("guest {vmid}: no default route after {secs}s");
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mount_lines() {
        let m = Mount::parse(
            "mp1",
            "local-lvm:vm-201-disk-1,mp=/mnt/frac,size=512M,backup=1",
        )
        .unwrap();
        assert_eq!(m.storage(), Some("local-lvm"));
        assert_eq!(m.mp.as_deref(), Some("/mnt/frac"));
        assert_eq!(m.size.unwrap().to_string(), "512M");
        assert!(m.backup);
        assert_eq!(m.index(), Some(1));
        assert_eq!(
            m.to_string(),
            "local-lvm:vm-201-disk-1,mp=/mnt/frac,size=512M,backup=1"
        );

        let b = Mount::parse("mp0", "/NetApp/FileStore/Media,mp=/mnt/media").unwrap();
        assert!(b.is_bind());
        assert_eq!(b.storage(), None);
    }

    #[test]
    fn parses_config_json() {
        let v: Value = serde_json::from_str(
            r#"{"arch":"amd64","cores":2,"features":"nesting=1,keyctl=1","hostname":"x","memory":1024,
                "mp0":"local-lvm:vm-9-disk-1,mp=/opt/stack,size=4G,backup=1","mp3":"/h,mp=/b",
                "rootfs":"local-lvm:vm-9-disk-0,size=6G","tags":"compose;x","unprivileged":1}"#,
        )
        .unwrap();
        let c = parse_config(9, &v).unwrap();
        assert!(c.unprivileged);
        assert_eq!(c.features.get("keyctl").map(String::as_str), Some("1"));
        assert_eq!(c.tags, vec!["compose", "x"]);
        assert_eq!(c.mounts.len(), 2);
        assert_eq!(c.free_mp_index(), 1);
        assert!(c.mount_at("/opt/stack").is_some());
        assert_eq!(c.rootfs.unwrap().size.unwrap().to_string(), "6G");
    }
}
