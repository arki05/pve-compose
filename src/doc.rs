//! The `compose` subtree of a guest's pve-meta document, read through the
//! `pve-meta` CLI as root on the node: no token, exact YAML types.
//!
//! ```yaml
//! compose:
//!   stack:  { owner: "1000:1000" }
//!   policy: { pct: auto, docker: auto, pull: manual }
//!   spec:   # the compose document, verbatim structure
//!     services: ...
//!     volumes:
//!       db:    { x-pve: { storage: NetApp, size: 20G } }
//!       media: { x-pve: { path: /NetApp/FileStore/Media, backup: false } }
//! ```
//!
//! Every key but `spec` is optional. Booleans are accepted as `true`/`false`
//! and as `1`/`0`, the spelling a JSON-mode write leaves behind.
//!
//! There is deliberately no `pct` section: cores, memory, extra features,
//! network and onboot are the guest's PVE config, edited there. The wrapper
//! settings this tool owns are derived, not configured: the `nesting` and
//! `keyctl` features docker needs, and the managed volumes. The stack itself
//! lives on the rootfs at `/opt/stack`; only `x-pve` volumes get disks.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};

use crate::cmd;
use crate::PREFIX;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Doc {
    #[serde(default)]
    pub stack: Stack,
    #[serde(default)]
    pub policy: Policy,
    pub spec: serde_yaml_ng::Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    /// `uid:gid` for volume directories and for `${PVE_UID}`/`${PVE_GID}`.
    pub owner: Option<String>,
}

/// When the daemon acts on its own. `Manual` means only a hand-run verb does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    Manual,
}

/// What the daemon may do without being asked, one flag per CLI level plus
/// image pulls, which are the one action that can change a running app
/// without the document changing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default = "auto")]
    pub pct: Mode,
    #[serde(default = "auto")]
    pub docker: Mode,
    #[serde(default = "manual")]
    pub pull: Mode,
}

fn auto() -> Mode {
    Mode::Auto
}
fn manual() -> Mode {
    Mode::Manual
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            pct: Mode::Auto,
            docker: Mode::Auto,
            pull: Mode::Manual,
        }
    }
}

pub fn flag_opt<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    let v = serde_yaml_ng::Value::deserialize(d)?;
    if v.is_null() {
        return Ok(None);
    }
    flag_of(&v)
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("expected a boolean (true/false or 1/0)"))
}

pub fn flag_of(v: &serde_yaml_ng::Value) -> Option<bool> {
    use serde_yaml_ng::Value as V;
    match v {
        V::Bool(b) => Some(*b),
        V::Number(n) => n.as_i64().and_then(|i| match i {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }),
        V::String(s) => match s.trim() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A document as read: the parsed subtree plus the exact text it came from
/// and that text's digest, which is what the facts file records as applied.
#[derive(Debug, Clone)]
pub struct Read {
    pub vmid: u32,
    pub text: String,
    pub digest: String,
    pub doc: Doc,
}

/// Reads guest `vmid`'s `compose` subtree. `Ok(None)` when the guest has no
/// such subtree (or no document at all).
pub fn read(vmid: u32) -> Result<Option<Read>> {
    let out = cmd::run_status(
        "pve-meta",
        &["get", &vmid.to_string(), PREFIX, "--format", "yaml"],
    )?;
    match out.status {
        0 => {}
        2 => return Ok(None),
        s => bail!(
            "pve-meta get {vmid} {PREFIX} failed (exit {s}): {}",
            out.stderr.trim()
        ),
    }
    let text = out.stdout;
    let doc = parse(&text).with_context(|| format!("guest {vmid}: bad compose document"))?;
    Ok(Some(Read {
        vmid,
        digest: digest(&text),
        text,
        doc,
    }))
}

/// Parses the YAML text of a `compose` subtree. Everything that later lands
/// in a shell line inside the guest (owner, project) is validated here, so
/// a document can never become a command.
pub fn parse(text: &str) -> Result<Doc> {
    let doc: Doc = serde_yaml_ng::from_str(text).context("compose subtree")?;
    if !doc.spec.is_mapping() {
        bail!("compose.spec must be a map (the compose document)");
    }
    if let Some(o) = &doc.stack.owner {
        crate::spec::split_owner(o).context("compose.stack.owner")?;
    }
    Ok(doc)
}

/// Compose's own rule for a project name.
pub fn is_project_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A hostname turned into a project name: lowercased, anything else `-`.
pub fn project_name_from(hostname: &str) -> String {
    let mut out: String = hostname
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '-' | '_' => c,
            _ => '-',
        })
        .collect();
    while out.starts_with(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit())) {
        out.remove(0);
    }
    if out.is_empty() {
        "stack".into()
    } else {
        out
    }
}

/// Writes `text` as guest `vmid`'s whole `compose` subtree.
pub fn write(vmid: u32, text: &str) -> Result<()> {
    write_prefix(vmid, PREFIX, text)
}

/// Writes `text` (YAML) as the whole `prefix` subtree of guest `vmid`'s
/// document, through `pve-meta set --file`.
pub fn write_prefix(vmid: u32, prefix: &str, text: &str) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!(
        "pve-compose-{prefix}-{vmid}-{}.yaml",
        std::process::id()
    ));
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    let res = cmd::run(
        "pve-meta",
        &[
            "set",
            &vmid.to_string(),
            prefix,
            "--file",
            &tmp.to_string_lossy(),
        ],
    );
    let _ = std::fs::remove_file(&tmp);
    res.map(|_| ())
}

pub fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_document() {
        let d = parse("spec:\n  services: {}\n").unwrap();
        assert_eq!(d.policy.pct, Mode::Auto);
        assert_eq!(d.policy.docker, Mode::Auto);
        assert_eq!(d.policy.pull, Mode::Manual);
        assert!(d.stack.owner.is_none());
    }

    #[test]
    fn flags_in_both_spellings() {
        let one: serde_yaml_ng::Value = serde_yaml_ng::from_str("1").unwrap();
        let t: serde_yaml_ng::Value = serde_yaml_ng::from_str("true").unwrap();
        let no: serde_yaml_ng::Value = serde_yaml_ng::from_str("\"no\"").unwrap();
        assert_eq!(flag_of(&one), Some(true));
        assert_eq!(flag_of(&t), Some(true));
        assert_eq!(flag_of(&no), Some(false));
    }

    #[test]
    fn policy_keys() {
        let d = parse("policy: { docker: manual }\nspec: {}\n").unwrap();
        assert_eq!(d.policy.pct, Mode::Auto);
        assert_eq!(d.policy.docker, Mode::Manual);
        assert!(parse("policy: { apply: auto }\nspec: {}\n").is_err());
    }

    #[test]
    fn unknown_key_is_refused() {
        assert!(parse("stak: {}\nspec: {}\n").is_err());
    }

    #[test]
    fn project_and_owner_are_validated() {
        assert!(parse("stack: { project: wiki }\nspec: {}\n").is_err());
        assert!(parse("stack: { storage: x }\nspec: {}\n").is_err());
        assert!(parse("stack: { owner: \"1000:1000\" }\nspec: {}\n").is_ok());
        assert!(parse("stack: { owner: \"root:root\" }\nspec: {}\n").is_err());
        assert_eq!(project_name_from("Wiki"), "wiki");
        assert_eq!(project_name_from("My Stack.local"), "my-stack-local");
        assert_eq!(project_name_from("--x"), "x");
        assert_eq!(project_name_from("!!!"), "stack");
    }

    #[test]
    fn spec_must_be_a_map() {
        assert!(parse("spec: 3\n").is_err());
        assert!(parse("policy: {}\n").is_err());
    }
}
