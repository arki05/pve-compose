//! The wrapper as PVE sees it. Reads go through `pvesh` (JSON), writes
//! through `pct`, and anything inside the guest through `pct exec` and
//! `pct push`. Nothing here knows about compose.

use std::collections::BTreeMap;
use std::fmt;

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

/// `pct set <vmid> <args>`.
pub fn set(vmid: u32, args: &[&str]) -> Result<()> {
    let vm = vmid.to_string();
    let mut all = vec!["set", vm.as_str()];
    all.extend_from_slice(args);
    cmd::run("pct", &all).map(|_| ())
}

/// `pct resize <vmid> <disk> <size>`.
pub fn resize(vmid: u32, disk: &str, size: Size) -> Result<()> {
    let vm = vmid.to_string();
    cmd::run("pct", &["resize", &vm, disk, &size.to_pct_resize()]).map(|_| ())
}

/// Runs a shell command line inside the guest, capturing output. A non-zero
/// exit is returned in `Output.status`, not as an error.
pub fn exec_status(vmid: u32, script: &str) -> Result<cmd::Output> {
    let vm = vmid.to_string();
    cmd::run_status("pct", &["exec", &vm, "--", "/bin/sh", "-c", script])
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

/// Runs a shell command line inside the guest with the terminal attached.
pub fn exec_stream(vmid: u32, script: &str) -> Result<()> {
    let vm = vmid.to_string();
    cmd::stream("pct", &["exec", &vm, "--", "/bin/sh", "-c", script])
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

/// Writes a file inside the guest with `pct push`, parent directories made
/// first. `perms` is octal text (`0644`).
pub fn write_file(vmid: u32, path: &str, content: &str, perms: &str) -> Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".into());
    exec(vmid, &format!("mkdir -p '{parent}'"))?;
    let tmp = std::env::temp_dir().join(format!(
        "pve-compose-push-{vmid}-{}-{}",
        std::process::id(),
        path.replace('/', "_")
    ));
    std::fs::write(&tmp, content).with_context(|| format!("cannot write {}", tmp.display()))?;
    let vm = vmid.to_string();
    let res = cmd::run(
        "pct",
        &["push", &vm, &tmp.to_string_lossy(), path, "--perms", perms],
    );
    let _ = std::fs::remove_file(&tmp);
    res.map(|_| ())
}

/// The guest's first global IPv4 address on eth0, if it has one.
pub fn ipv4(vmid: u32) -> Result<Option<String>> {
    let out = exec_status(
        vmid,
        "ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | head -n1",
    )?;
    if out.status != 0 {
        return Ok(None);
    }
    Ok(out
        .stdout
        .trim()
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(String::from))
}

pub fn start(vmid: u32) -> Result<()> {
    cmd::run("pct", &["start", &vmid.to_string()]).map(|_| ())
}

/// `pct reboot`: the way a changed `features` line takes effect.
pub fn reboot(vmid: u32) -> Result<()> {
    cmd::run("pct", &["reboot", &vmid.to_string()]).map(|_| ())
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

/// Waits until the guest answers `pct exec` and has a global IPv4 address,
/// for up to `secs` seconds. Returns the address.
pub fn wait_ready(vmid: u32, secs: u32) -> Result<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs as u64);
    loop {
        if let Ok(Some(ip)) = ipv4(vmid) {
            return Ok(ip);
        }
        if std::time::Instant::now() >= deadline {
            bail!("guest {vmid}: no network after {secs}s");
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
