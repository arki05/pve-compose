//! `/etc/pve/pve-compose.cfg`: the operator's own settings. Cluster-wide
//! (pmxcfs), one YAML file, every key optional.
//!
//! Per-guest knobs the document may set have their *defaults* here, once
//! for the cluster and overridable per node, because storage names differ per
//! node. Precedence for such a knob is: document, node, cluster default,
//! built-in.
//!
//! ```yaml
//! template:
//!   storage: local                 # a storage with vztmpl content, for the base template
//!   base: debian-13-standard       # the upstream template new unpacks
//! defaults:
//!   storage: local-lvm             # stack disk and managed volumes
//!   stack_size: 4G
//!   rootfs_size: 8G
//!   owner: "1000:1000"
//!   bridge: vmbr0
//!   cores: 2
//!   memory: 2048
//!   swap: 0
//! nodes:
//!   StorageCube: { storage: NetApp }
//! daemon:
//!   interval: 30                   # seconds between version-token polls
//!   full_every: 600                # seconds between unconditional passes
//! ```

use std::collections::BTreeMap;
use std::fs;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::size::Size;

pub const PATH: &str = "/etc/pve/pve-compose.cfg";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub template: Template,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub nodes: BTreeMap<String, NodeOverrides>,
    #[serde(default)]
    pub daemon: Daemon,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    #[serde(default = "d_template_storage")]
    pub storage: String,
    #[serde(default = "d_template_base")]
    pub base: String,
}

fn d_template_storage() -> String {
    "local".into()
}
fn d_template_base() -> String {
    "debian-13-standard".into()
}

impl Default for Template {
    fn default() -> Self {
        Template {
            storage: d_template_storage(),
            base: d_template_base(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default = "d_storage")]
    pub storage: String,
    #[serde(default = "d_stack_size")]
    pub stack_size: Size,
    #[serde(default = "d_rootfs_size")]
    pub rootfs_size: Size,
    #[serde(default = "d_owner")]
    pub owner: String,
    #[serde(default = "d_bridge")]
    pub bridge: String,
    #[serde(default = "d_cores")]
    pub cores: u32,
    #[serde(default = "d_memory")]
    pub memory: u64,
    #[serde(default)]
    pub swap: u64,
}

fn d_storage() -> String {
    "local-lvm".into()
}
fn d_stack_size() -> Size {
    "4G".parse().unwrap()
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
            storage: d_storage(),
            stack_size: d_stack_size(),
            rootfs_size: d_rootfs_size(),
            owner: d_owner(),
            bridge: d_bridge(),
            cores: d_cores(),
            memory: d_memory(),
            swap: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NodeOverrides {
    pub storage: Option<String>,
    pub bridge: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Daemon {
    #[serde(default = "d_interval")]
    pub interval: u64,
    #[serde(default = "d_full_every")]
    pub full_every: u64,
}

fn d_interval() -> u64 {
    30
}
fn d_full_every() -> u64 {
    600
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

    /// The default storage for managed disks on `node`.
    pub fn storage_for(&self, node: &str) -> String {
        self.nodes
            .get(node)
            .and_then(|n| n.storage.clone())
            .unwrap_or_else(|| self.defaults.storage.clone())
    }

    /// The default bridge on `node`.
    pub fn bridge_for(&self, node: &str) -> String {
        self.nodes
            .get(node)
            .and_then(|n| n.bridge.clone())
            .unwrap_or_else(|| self.defaults.bridge.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let c: Config = serde_yaml_ng::from_str("defaults: {}\n").unwrap();
        assert_eq!(c.defaults.storage, "local-lvm");
        assert_eq!(c.daemon.interval, 30);
        assert_eq!(c.storage_for("x"), "local-lvm");
    }

    #[test]
    fn node_override_wins() {
        let c: Config = serde_yaml_ng::from_str("nodes:\n  a: { storage: NetApp }\n").unwrap();
        assert_eq!(c.storage_for("a"), "NetApp");
        assert_eq!(c.storage_for("b"), "local-lvm");
    }
}
