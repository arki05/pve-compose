//! `new`: a wrapper from the template, tagged, with an empty document,
//! started, docker installed, applied. The stack is yours to write.
//!
//! Everything it needs to know is settled before it touches anything: a
//! storage for the template and one for the wrapper's disks come from a
//! flag, from the persisted config, or from a question at the terminal, in
//! that order, and a question's answer is offered for persisting. Only then
//! is the guest created.

use anyhow::{bail, Result};

use crate::cmd;
use crate::config::{self, Config};
use crate::doc;
use crate::ops::{self, apply, template, Ctx};
use crate::pct;
use crate::prompt;
use crate::size::Size;
use crate::stack;

/// Everything `pct create` needs.
pub struct WrapperSpec {
    pub vmid: u32,
    pub hostname: String,
    pub template: String,
    pub rootfs_storage: String,
    pub rootfs_size: Size,
    pub unprivileged: bool,
    pub cores: u32,
    pub memory: u64,
    pub net0: String,
    pub features: String,
    pub tags: Vec<String>,
    pub onboot: bool,
}

/// Creates the wrapper, not started.
pub fn create_wrapper(w: &WrapperSpec) -> Result<()> {
    let vm = w.vmid.to_string();
    let rootfs = format!("{}:{}", w.rootfs_storage, w.rootfs_size.to_pct_gib());
    let cores = w.cores.to_string();
    let memory = w.memory.to_string();
    let unpriv = if w.unprivileged { "1" } else { "0" };
    let onboot = if w.onboot { "1" } else { "0" };
    let tags = w.tags.join(";");
    let mut args: Vec<&str> = vec![
        "create",
        &vm,
        &w.template,
        "--hostname",
        &w.hostname,
        "--rootfs",
        &rootfs,
        "--unprivileged",
        unpriv,
        "--cores",
        &cores,
        "--memory",
        &memory,
        "--swap",
        "0",
        "--net0",
        &w.net0,
        "--features",
        &w.features,
        "--onboot",
        onboot,
    ];
    if !tags.is_empty() {
        args.push("--tags");
        args.push(&tags);
    }
    // Unpacking a template takes minutes on a slow storage.
    cmd::run_within("pct", &args, cmd::LONG_TIMEOUT).map(|_| ())
}

pub struct NewArgs {
    pub vmid: u32,
    pub name: String,
    pub template: Option<String>,
    pub storage: Option<String>,
    pub rootfs_size: Option<Size>,
    pub bridge: Option<String>,
    pub ip: Option<String>,
    pub gateway: Option<String>,
    pub cores: Option<u32>,
    pub memory: Option<u64>,
    pub no_up: bool,
}

/// The document a fresh wrapper gets: the default policy and an empty
/// `spec`, nothing to run until you write the stack.
pub fn initial_document() -> &'static str {
    "policy:\n  pct: auto\n  docker: auto\n  pull: manual\nspec:\n  services: {}\n"
}

/// Where a settled value came from, for the persist question.
enum Source {
    Flag,
    Config,
    Asked,
}

/// A storage settled from a flag, the config, or a question. `content` is
/// what it must accept (`rootdir` or `vztmpl`); `what` names it to the user.
fn settle_storage(
    ctx: &Ctx,
    flag: Option<&str>,
    configured: Option<&str>,
    content: &str,
    what: &str,
    config_key: &str,
) -> Result<(String, Source)> {
    if let Some(s) = flag {
        return Ok((s.to_string(), Source::Flag));
    }
    if let Some(s) = configured {
        return Ok((s.to_string(), Source::Config));
    }
    let candidates = pct::storages(&ctx.node, content)?;
    if candidates.is_empty() {
        bail!(
            "no enabled storage on {} accepts `{content}` content; allow it on one (Datacenter > Storage > Edit > Content), then run again",
            ctx.node
        );
    }
    if !prompt::interactive() {
        let names: Vec<&str> = candidates.iter().map(|s| s.id.as_str()).collect();
        bail!(
            "no {what} configured and no terminal to ask on; `pve-compose config set {config_key} <storage>` (on {}: {})",
            ctx.node,
            names.join(", ")
        );
    }
    let i = prompt::choose(
        &format!("Which storage for {what} on {}?", ctx.node),
        &candidates,
        |s| {
            format!(
                "{:<16} {:<10} {:>8} free{}",
                s.id,
                s.kind,
                Size(s.avail).human(),
                if s.shared { "  (shared)" } else { "" }
            )
        },
    )?;
    Ok((candidates[i].id.clone(), Source::Asked))
}

pub fn new(ctx: &Ctx, a: &NewArgs) -> Result<()> {
    let vmid = a.vmid;
    if pct::owner_node(vmid)?.is_some() {
        bail!("vmid {vmid} is already in use");
    }

    // Everything to decide, decided; nothing touched yet.
    let explicit_volid = a.template.as_deref().filter(|t| t.contains(":vztmpl/"));
    let (template_storage, template_source) = match explicit_volid {
        Some(v) => (v.split(':').next().unwrap_or("").to_string(), Source::Flag),
        None => settle_storage(
            ctx,
            None,
            ctx.config.template.storage.as_deref(),
            "vztmpl",
            "templates",
            "template-storage",
        )?,
    };
    let (storage, storage_source) = settle_storage(
        ctx,
        a.storage.as_deref(),
        ctx.config.storage_for(&ctx.node).as_deref(),
        "rootdir",
        "container disks",
        "storage",
    )?;
    let store_template = matches!(template_source, Source::Asked)
        && prompt::yes_no(
            &format!("Store {template_storage} as the template storage for the cluster?"),
            true,
        )?;
    let store_storage = matches!(storage_source, Source::Asked)
        && prompt::yes_no(
            &format!("Store {storage} as the disk storage for {}?", ctx.node),
            true,
        )?;
    let cfg = if store_template || store_storage {
        // Applied to the file as it is now, not as it was loaded before the
        // questions: another node may have written it meanwhile.
        let c = Config::update(|c| {
            if store_template {
                c.template.storage = Some(template_storage.clone());
            }
            if store_storage {
                c.set_storage_for(&ctx.node, Some(storage.clone()));
            }
        })?;
        eprintln!("new: stored in {}", config::PATH);
        c
    } else {
        ctx.config.clone()
    };

    let template = template::resolve(ctx, &template_storage, a.template.as_deref())?;
    let bridge = a
        .bridge
        .clone()
        .unwrap_or_else(|| cfg.defaults.bridge.clone());
    let ip = a.ip.clone().unwrap_or_else(|| "dhcp".into());
    let mut net0 = format!("name=eth0,bridge={bridge},ip={ip}");
    if let Some(gw) = &a.gateway {
        net0.push_str(&format!(",gw={gw}"));
    }
    let w = WrapperSpec {
        vmid,
        hostname: a.name.clone(),
        template: template.clone(),
        rootfs_storage: storage,
        rootfs_size: a.rootfs_size.unwrap_or(cfg.defaults.rootfs_size),
        unprivileged: true,
        cores: a.cores.unwrap_or(cfg.defaults.cores),
        memory: a.memory.unwrap_or(cfg.defaults.memory),
        net0,
        features: "nesting=1,keyctl=1".into(),
        tags: ctx.tag.iter().cloned().collect(),
        onboot: true,
    };
    eprintln!("new: creating {vmid} ({}) from {template}", a.name);
    create_wrapper(&w)?;

    doc::write(vmid, initial_document())?;
    eprintln!("new: document written");

    // Hold the lock from the start, so the daemon's first pass waits for
    // `new` to finish instead of racing it to the first apply.
    let _lock = crate::lock::GuestLock::take(vmid, 60)?;
    pct::start(vmid)?;
    pct::wait_ready(vmid, 90)?;
    eprintln!("new: {vmid} is up");
    apply::apply_locked(
        ctx,
        vmid,
        apply::Options {
            no_up: a.no_up,
            ..Default::default()
        },
    )?;
    stack::write_facts(
        vmid,
        &stack::Facts {
            template: Some(template),
            ..stack::read_facts(vmid)?.unwrap_or_default()
        },
    )?;
    eprintln!(
        "new: done; write the stack into {vmid}'s `compose.spec` (Metadata tab, or pve-meta set {vmid} compose.spec), data under /opt/stack/volumes/, secrets in /opt/stack/.env{}",
        if a.no_up { ", then `pve-compose docker apply {vmid}`" } else { "" }
    );
    ops::require_running(ctx, vmid)
}
