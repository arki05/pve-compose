//! Inside the guest: the stack directory, the facts file, and running compose.
//!
//! ```text
//! /opt/stack/                     on the rootfs
//!   compose.yaml                  rendered from the document (spec::render)
//!   .env                          the user's, never touched
//!   volumes/<name>/               one per compose volume
//!   .pve-compose/
//!     applied.yaml                Facts: what the last apply put here
//! ```
//!
//! Compose is always run as `cd /opt/stack && docker compose ...`: the
//! rendered file carries the project name as its top-level `name:`, so a
//! human typing the same thing gets the same stack.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::pct;
use crate::{FACTS_DIR, STACK_DIR, VOLUMES_DIR};

pub const COMPOSE_FILE: &str = "/opt/stack/compose.yaml";
pub const ENV_FILE: &str = "/opt/stack/.env";
pub const FACTS_FILE: &str = "/opt/stack/.pve-compose/applied.yaml";

/// What the last apply recorded. Facts, not intent: the document never
/// holds these (`docs/DESIGN.md`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Facts {
    /// Digest of the document text that was applied.
    pub digest: String,
    /// The template volid this wrapper was built from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// When, RFC 3339.
    pub applied_at: String,
    /// pve-compose's version that applied it.
    pub by: String,
}

pub fn read_facts(vmid: u32) -> Result<Option<Facts>> {
    let Some(text) = pct::read_file(vmid, FACTS_FILE)? else {
        return Ok(None);
    };
    let f: Facts = serde_yaml_ng::from_str(&text)
        .with_context(|| format!("guest {vmid}: {FACTS_FILE} is not a facts file"))?;
    Ok(Some(f))
}

pub fn write_facts(vmid: u32, facts: &Facts) -> Result<()> {
    let text = serde_yaml_ng::to_string(facts)?;
    pct::write_file(vmid, FACTS_FILE, &text, "0644")
}

/// New facts for an apply, keeping what the previous facts knew about the
/// wrapper itself.
pub fn facts_now(digest: &str, previous: Option<Facts>) -> Facts {
    let prev = previous.unwrap_or_default();
    Facts {
        digest: digest.to_string(),
        template: prev.template,
        applied_at: chrono::Local::now().to_rfc3339(),
        by: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Makes the stack layout exist, with the volume directories owned as asked.
/// Owners were validated as `uid:gid` when the document was parsed, and
/// directory names when the volumes were.
pub fn ensure_layout(vmid: u32, owners: &[(String, String)]) -> Result<()> {
    let mut script = format!("set -e; mkdir -p {VOLUMES_DIR} {FACTS_DIR}; chmod 0755 {STACK_DIR};");
    for (dir, owner) in owners {
        // Only chown a directory that is still root's: a running stack owns
        // its files, and an owner change under it is not this tool's call.
        script.push_str(&format!(
            " mkdir -p '{dir}'; if [ \"$(stat -c %u:%g '{dir}')\" = '0:0' ]; then chown '{owner}' '{dir}'; fi;"
        ));
    }
    pct::exec(vmid, &script).map(|_| ())
}

/// The compose file currently in the guest, if any.
pub fn read_compose(vmid: u32) -> Result<Option<String>> {
    pct::read_file(vmid, COMPOSE_FILE)
}

pub fn write_compose(vmid: u32, text: &str) -> Result<()> {
    pct::write_file(vmid, COMPOSE_FILE, text, "0644")
}

fn compose() -> String {
    format!("cd {STACK_DIR} && docker compose")
}

/// `docker compose up -d --remove-orphans`, output on the terminal.
pub fn up(vmid: u32) -> Result<()> {
    pct::exec_stream(vmid, &format!("{} up -d --remove-orphans", compose()))
}

/// `docker compose pull`, output on the terminal.
pub fn pull(vmid: u32) -> Result<()> {
    pct::exec_stream(vmid, &format!("{} pull", compose()))
}

/// Any `docker compose` command line, output on the terminal.
pub fn passthrough(vmid: u32, args: &[String]) -> Result<()> {
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    pct::exec_stream(vmid, &format!("{} {}", compose(), quoted.join(" ")))
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// One container of the stack, from `docker compose ps`.
#[derive(Debug, Clone, Deserialize)]
pub struct Container {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Service", default)]
    pub service: String,
    #[serde(rename = "State", default)]
    pub state: String,
    #[serde(rename = "Status", default)]
    pub status: String,
}

/// The stack's containers. Compose prints either a JSON array or one object
/// per line depending on its version; both are read.
pub fn ps(vmid: u32) -> Result<Vec<Container>> {
    let out = pct::exec(vmid, &format!("{} ps -a --format json", compose()))?;
    let text = out.stdout.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(list) = serde_json::from_str::<Vec<Container>>(text) {
        return Ok(list);
    }
    let mut list = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        list.push(serde_json::from_str::<Container>(line).context("docker compose ps output")?);
    }
    Ok(list)
}

/// Whether docker is installed and answering inside the guest.
pub fn docker_ok(vmid: u32) -> Result<bool> {
    let out = pct::exec_status(vmid, "docker info >/dev/null 2>&1")?;
    Ok(out.status == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("ps"), "ps");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn ps_both_shapes() {
        let arr = r#"[{"Name":"a","Service":"s","State":"running","Status":"Up"}]"#;
        let v: Vec<Container> = serde_json::from_str(arr).unwrap();
        assert_eq!(v[0].service, "s");
    }
}
