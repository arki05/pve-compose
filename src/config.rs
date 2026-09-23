//! `/etc/pve/pve-compose.cfg`: the operator's persisted choices. Cluster-wide
//! (pmxcfs), one YAML file, every key optional, and nobody has to open it:
//! `new` asks for what it lacks and offers to store the answer, and
//! `pve-compose config set|unset` writes the same file.
//!
//! ```yaml
//! template:
//!   storage: local                 # a storage with vztmpl content, cluster-wide
//!   base: debian-13-standard       # the upstream template `new` unpacks
//! defaults:
//!   rootfs_size: 8G
//!   owner: "1000:1000"
//!   bridge: vmbr0
//!   cores: 2
//!   memory: 2048
//! nodes:
//!   StorageCube: { storage: NetApp }   # disks on this node; storage names are per node
//! limits:
//!   bind_roots: [/NetApp/FileStore]   # what an x-pve.path may be under; empty: no binds
//!   storages: [NetApp]                # what an x-pve.storage may name; empty: the node's own
//!   max_disk_gib: 64                  # the largest disk one x-pve.size may ask for
//! daemon:
//!   interval: 30                   # seconds between version-token polls
//!   full_every: 600                # seconds between unconditional passes
//! ```
//!
//! There is deliberately no cluster-wide disk storage: storage names differ
//! per node, and a cluster default would be a guess.

use std::collections::BTreeMap;
use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::size::Size;

pub const PATH: &str = "/etc/pve/pve-compose.cfg";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "is_default")]
    pub template: Template,
    #[serde(default, skip_serializing_if = "is_default")]
    pub defaults: Defaults,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<String, NodeOverrides>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub limits: Limits,
    #[serde(default, skip_serializing_if = "is_default")]
    pub daemon: Daemon,
}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    *t == T::default()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Template {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(
        default = "d_template_base",
        skip_serializing_if = "is_d_template_base"
    )]
    pub base: String,
}

fn d_template_base() -> String {
    "debian-13-standard".into()
}
fn is_d_template_base(v: &String) -> bool {
    *v == d_template_base()
}

impl Default for Template {
    fn default() -> Self {
        Template {
            storage: None,
            base: d_template_base(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default = "d_rootfs_size", skip_serializing_if = "is_d_rootfs_size")]
    pub rootfs_size: Size,
    #[serde(default = "d_owner", skip_serializing_if = "is_d_owner")]
    pub owner: String,
    #[serde(default = "d_bridge", skip_serializing_if = "is_d_bridge")]
    pub bridge: String,
    #[serde(default = "d_cores", skip_serializing_if = "is_d_cores")]
    pub cores: u32,
    #[serde(default = "d_memory", skip_serializing_if = "is_d_memory")]
    pub memory: u64,
}

fn is_d_rootfs_size(v: &Size) -> bool {
    *v == d_rootfs_size()
}
fn is_d_owner(v: &String) -> bool {
    *v == d_owner()
}
fn is_d_bridge(v: &String) -> bool {
    *v == d_bridge()
}
fn is_d_cores(v: &u32) -> bool {
    *v == d_cores()
}
fn is_d_memory(v: &u64) -> bool {
    *v == d_memory()
}

fn d_rootfs_size() -> Size {
    "8G".parse().unwrap()
}
fn d_owner() -> String {
    "1000:1000".into()
}
fn d_bridge() -> String {
    "vmbr0".into()
}
fn d_cores() -> u32 {
    2
}
fn d_memory() -> u64 {
    2048
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            rootfs_size: d_rootfs_size(),
            owner: d_owner(),
            bridge: d_bridge(),
            cores: d_cores(),
            memory: d_memory(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
}

/// What a guest's document may ask the node for. Writing a document takes
/// `VM.Config.Options` on that guest, which is not what PVE asks for a bind
/// mount (`root@pam` only) or for allocating on any storage, so `x-pve` is
/// held to these bounds (README, "Trust"). Hand-edited: `config set` does
/// not write them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Host directories an `x-pve.path` may be, or be under. Empty (the
    /// default): no document gets a bind mount.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bind_roots: Vec<String>,
    /// Storages an `x-pve.storage` may name. Empty (the default): only the
    /// storage the node is configured with, which is where a volume without
    /// a storage lands anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storages: Vec<String>,
    /// The largest disk one `x-pve.size` may ask for, in GiB.
    #[serde(default = "d_max_disk_gib", skip_serializing_if = "is_d_max_disk_gib")]
    pub max_disk_gib: u64,
}

fn d_max_disk_gib() -> u64 {
    64
}
fn is_d_max_disk_gib(v: &u64) -> bool {
    *v == d_max_disk_gib()
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            bind_roots: Vec::new(),
            storages: Vec::new(),
            max_disk_gib: d_max_disk_gib(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Daemon {
    #[serde(default = "d_interval", skip_serializing_if = "is_d_interval")]
    pub interval: u64,
    #[serde(default = "d_full_every", skip_serializing_if = "is_d_full_every")]
    pub full_every: u64,
}

fn d_interval() -> u64 {
    30
}
fn d_full_every() -> u64 {
    600
}
fn is_d_interval(v: &u64) -> bool {
    *v == d_interval()
}
fn is_d_full_every(v: &u64) -> bool {
    *v == d_full_every()
}

impl Default for Daemon {
    fn default() -> Self {
        Daemon {
            interval: d_interval(),
            full_every: d_full_every(),
        }
    }
}

impl Config {
    /// Loads the file, or the built-in defaults when it does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(PATH)
    }

    pub fn load_from(path: &str) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                if text.trim().is_empty() {
                    return Ok(Config::default());
                }
                serde_yaml_ng::from_str(&text).with_context(|| format!("{path}: bad configuration"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {path}")),
        }
    }

    /// Writes the file: only what differs from the defaults, so the file
    /// says exactly what was chosen.
    pub fn save(&self) -> Result<()> {
        self.save_to(PATH)
    }

    /// Re-reads the file and applies `change` to what is there now, then
    /// saves: a writer that loaded the config a while ago (a `new` that
    /// waited on a question) must not clobber what another node wrote in
    /// between.
    pub fn update(change: impl FnOnce(&mut Config)) -> Result<Config> {
        let mut c = Config::load()?;
        change(&mut c);
        c.save()?;
        Ok(c)
    }

    pub fn save_to(&self, path: &str) -> Result<()> {
        let text = if *self == Config::default() {
            String::new()
        } else {
            serde_yaml_ng::to_string(self)?
        };
        let body = format!(
            "# pve-compose: written by `pve-compose config` and `new`; every key optional.\n{text}"
        );
        let tmp = format!("{path}.tmp");
        fs::write(&tmp, body).with_context(|| format!("cannot write {tmp}"))?;
        fs::rename(&tmp, path).with_context(|| format!("cannot replace {path}"))
    }

    /// The persisted disk storage for `node`, if any.
    pub fn storage_for(&self, node: &str) -> Option<String> {
        self.nodes.get(node).and_then(|n| n.storage.clone())
    }

    /// The bounds a document on `node` is held to, with an empty storage
    /// allow-list resolved to that node's own disk storage: a document may
    /// name the storage its volumes would land on anyway, and nothing else
    /// until `limits.storages` says so.
    pub fn limits_for(&self, node: &str) -> Limits {
        let mut l = self.limits.clone();
        if l.storages.is_empty() {
            l.storages = self.storage_for(node).into_iter().collect();
        }
        l
    }

    pub fn set_storage_for(&mut self, node: &str, storage: Option<String>) {
        match storage {
            Some(s) => {
                self.nodes.entry(node.to_string()).or_default().storage = Some(s);
            }
            None => {
                if let Some(n) = self.nodes.get_mut(node) {
                    n.storage = None;
                    if *n == NodeOverrides::default() {
                        self.nodes.remove(node);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let c: Config = serde_yaml_ng::from_str("defaults: {}\n").unwrap();
        assert_eq!(c.defaults.owner, "1000:1000");
        assert_eq!(c.daemon.interval, 30);
        assert_eq!(c.storage_for("x"), None);
        assert_eq!(c.template.storage, None);
    }

    #[test]
    fn node_storage_is_per_node() {
        let c: Config = serde_yaml_ng::from_str("nodes:\n  a: { storage: NetApp }\n").unwrap();
        assert_eq!(c.storage_for("a").as_deref(), Some("NetApp"));
        assert_eq!(c.storage_for("b"), None);
    }

    #[test]
    fn save_writes_only_choices_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("pve-compose-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.yaml");
        let path = path.to_str().unwrap();
        let mut c = Config::default();
        c.save_to(path).unwrap();
        assert_eq!(Config::load_from(path).unwrap(), Config::default());
        assert!(!std::fs::read_to_string(path)
            .unwrap()
            .contains("rootfs_size"));
        c.set_storage_for("n1", Some("NetApp".into()));
        c.template.storage = Some("local".into());
        c.save_to(path).unwrap();
        let back = Config::load_from(path).unwrap();
        assert_eq!(back, c);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("NetApp") && text.contains("local"));
        assert!(!text.contains("cores") && !text.contains("owner") && !text.contains("base"));
        c.defaults.cores = 3;
        c.save_to(path).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("cores: 3") && !text.contains("memory"));
        c.set_storage_for("n1", None);
        assert!(c.nodes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(serde_yaml_ng::from_str::<Config>("defaults: { storage: x }\n").is_err());
        assert!(serde_yaml_ng::from_str::<Config>("defaults: { stack_size: 4G }\n").is_err());
    }
}
