//! The verbs. Each takes a [`Ctx`] and a guest and does one thing a human
//! could also do by hand with `pct`, `pve-meta` and `docker compose`.

pub mod apply;
pub mod configcmd;
pub mod daemon;
pub mod diff;
pub mod new;
pub mod provision;
pub mod status;
pub mod template;
pub mod upgrade;

use anyhow::{bail, Context as _, Result};

use crate::config::Config;
use crate::doc;
use crate::pct::{self, GuestConfig, Status};
use crate::spec::{self, Volume};

/// What every verb needs: which node this is, the operator's config, and
/// which guests the `compose` prefix reaches.
pub struct Ctx {
    pub node: String,
    pub config: Config,
    /// The tag the prefix's selector names, or `None` for `{ all: true }`:
    /// then any guest with a document is managed and no tag is added.
    pub tag: Option<String>,
}

impl Ctx {
    pub fn load() -> Result<Self> {
        Ok(Ctx {
            node: crate::cmd::nodename()?,
            config: Config::load()?,
            tag: selector_tag()?,
        })
    }

    /// What a document on this node may ask for (`config::Limits`).
    pub fn limits(&self) -> crate::config::Limits {
        self.config.limits_for(&self.node)
    }

    /// Whether a guest with these tags is one the prefix reaches.
    pub fn selected(&self, tags: &[String]) -> bool {
        match &self.tag {
            Some(t) => tags.iter().any(|x| x == t),
            None => true,
        }
    }
}

/// The selector of the `compose` prefix, read from the prefix file itself:
/// the cluster override if there is one, else the packaged file. One source,
/// never a second copy in this tool's config, and no API call to get it.
fn selector_tag() -> Result<Option<String>> {
    let name = format!("{}.yaml", crate::PREFIX);
    let candidates = [
        format!("/etc/pve/meta.d/prefixes/{name}"),
        format!("/usr/share/pve-meta/prefixes/{name}"),
    ];
    for path in &candidates {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {path}")),
        };
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&text).with_context(|| format!("{path}: not YAML"))?;
        let sel = v.get("selector").cloned().unwrap_or_default();
        if let Some(t) = sel.get("tag").and_then(|t| t.as_str()) {
            return Ok(Some(t.to_string()));
        }
        if sel.get("all").and_then(doc::flag_of) == Some(true) {
            return Ok(None);
        }
        bail!("{path}: selector is neither {{ tag: <t> }} nor {{ all: true }}");
    }
    bail!(
        "no prefix file for `{}` under /etc/pve/meta.d/prefixes or /usr/share/pve-meta/prefixes (is pve-compose installed correctly?)",
        crate::PREFIX
    )
}

/// A guest with its config and document loaded, the input of most verbs.
pub struct Guest {
    pub config: GuestConfig,
    pub read: doc::Read,
    pub volumes: Vec<Volume>,
}

impl Guest {
    pub fn vmid(&self) -> u32 {
        self.config.vmid
    }

    /// The compose project name: the hostname made into one.
    pub fn project(&self) -> String {
        match &self.config.hostname {
            Some(h) => doc::project_name_from(h),
            None => format!("ct{}", self.vmid()),
        }
    }

    /// The default owner for volume directories.
    pub fn owner(&self, ctx: &Ctx) -> String {
        self.read
            .doc
            .stack
            .owner
            .clone()
            .unwrap_or_else(|| ctx.config.defaults.owner.clone())
    }
}

/// Loads a guest on this node that has a compose document.
pub fn load(ctx: &Ctx, vmid: u32) -> Result<Guest> {
    let read = doc::read(vmid)?.with_context(|| {
        format!(
            "guest {vmid} has no compose document (no `{}` subtree)",
            crate::PREFIX
        )
    })?;
    load_with(ctx, read)
}

/// Loads the config and volumes for a document already read.
pub fn load_with(ctx: &Ctx, read: doc::Read) -> Result<Guest> {
    let vmid = read.vmid;
    let config = pct::config(&ctx.node, vmid)?;
    let volumes = spec::volumes(&read.doc.spec, &ctx.limits())
        .with_context(|| format!("guest {vmid}: compose.spec"))?;
    Ok(Guest {
        config,
        read,
        volumes,
    })
}

/// The same guest with its PVE config read again: after the pct half changed
/// mounts, the docker half must see them, and the document has not moved.
pub fn refresh_config(ctx: &Ctx, g: Guest) -> Result<Guest> {
    Ok(Guest {
        config: pct::config(&ctx.node, g.config.vmid)?,
        ..g
    })
}

/// Fails unless the guest is running. Stopped guests are never touched
/// (docs/DESIGN.md): start it, then apply.
pub fn require_running(ctx: &Ctx, vmid: u32) -> Result<()> {
    match pct::status(&ctx.node, vmid)? {
        Status::Running => Ok(()),
        Status::Stopped => bail!(
            "guest {vmid} is stopped; pve-compose only acts on running guests (pct start {vmid})"
        ),
        Status::Other => bail!("guest {vmid} is in an unexpected state"),
    }
}
