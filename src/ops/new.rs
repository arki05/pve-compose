//! `new`: a wrapper from the template, tagged, with a hello-world document,
//! started and applied.

use anyhow::{bail, Result};

use crate::cmd;
use crate::doc;
use crate::ops::{self, apply, template, Ctx};
use crate::pct;
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
    pub swap: u64,
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
    let swap = w.swap.to_string();
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
        &swap,
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
    cmd::run("pct", &args).map(|_| ())
}

pub struct NewArgs {
    pub vmid: u32,
    pub name: String,
    pub template: Option<String>,
    pub storage: Option<String>,
    pub rootfs_size: Option<Size>,
    pub stack_size: Option<Size>,
    pub bridge: Option<String>,
    pub ip: Option<String>,
    pub gateway: Option<String>,
    pub cores: Option<u32>,
    pub memory: Option<u64>,
    pub route: Option<String>,
    pub no_up: bool,
}

/// The document a fresh wrapper gets: a whoami container on port 8080, so
/// the whole chain is testable with one curl. Only what was given explicitly
/// is written under `stack`, so the document stays portable across nodes
/// with different storage names.
pub fn hello_world(a: &NewArgs) -> String {
    let mut s = String::new();
    if a.storage.is_some() || a.stack_size.is_some() {
        s.push_str("stack:\n");
        if let Some(st) = &a.storage {
            s.push_str(&format!("  storage: {st}\n"));
        }
        if let Some(sz) = a.stack_size {
            s.push_str(&format!("  size: {sz}\n"));
        }
    }
    s.push_str(
        "policy:\n  pct: auto\n  docker: auto\n  pull: manual\n\
         spec:\n  services:\n    hello:\n      image: traefik/whoami\n      restart: unless-stopped\n      ports:\n        - \"8080:80\"\n      environment:\n        WHOAMI_NAME: ${PVE_NAME}\n",
    );
    s
}

pub fn new(ctx: &Ctx, a: &NewArgs) -> Result<()> {
    let vmid = a.vmid;
    if pct::owner_node(vmid)?.is_some() {
        bail!("vmid {vmid} is already in use");
    }
    let template = template::resolve(ctx, a.template.as_deref())?;
    let storage = a
        .storage
        .clone()
        .unwrap_or_else(|| ctx.config.storage_for(&ctx.node));
    let bridge = a
        .bridge
        .clone()
        .unwrap_or_else(|| ctx.config.bridge_for(&ctx.node));
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
        rootfs_size: a.rootfs_size.unwrap_or(ctx.config.defaults.rootfs_size),
        unprivileged: true,
        cores: a.cores.unwrap_or(ctx.config.defaults.cores),
        memory: a.memory.unwrap_or(ctx.config.defaults.memory),
        swap: ctx.config.defaults.swap,
        net0,
        features: "nesting=1,keyctl=1".into(),
        tags: ctx.tag.iter().cloned().collect(),
        onboot: true,
    };
    eprintln!("new: creating {vmid} ({}) from {template}", a.name);
    create_wrapper(&w)?;

    doc::write(vmid, &hello_world(a))?;
    if let Some(host) = &a.route {
        write_route(vmid, &a.name, host)?;
    }
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
    if a.no_up {
        eprintln!("new: done; put the data under /opt/stack/volumes/ and your .env in /opt/stack/, then `pve-compose docker apply {vmid}`");
    } else {
        eprintln!(
            "new: done; the hello-world answers on port 8080 of {}",
            a.name
        );
    }
    ops::require_running(ctx, vmid)
}

/// A `traefik` subtree routing `host` to the hello-world's port 8080.
fn write_route(vmid: u32, name: &str, host: &str) -> Result<()> {
    let route = format!(
        "http:\n  routers:\n    {name}:\n      rule: Host(`{host}`)\n      entryPoints: [websecure]\n      tls: {{ certResolver: default }}\n      service: {name}\n  services:\n    {name}:\n      loadBalancer:\n        servers: [{{ port: 8080 }}]\n"
    );
    doc::write_prefix(vmid, "traefik", &route)
}
