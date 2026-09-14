//! `config`: the persisted choices, shown or changed, without opening the
//! file. Storage is per node; everything else is cluster-wide.

use anyhow::{bail, Result};

use crate::config::{self, Config};
use crate::ops::Ctx;

const KEYS: &[(&str, &str)] = &[
    (
        "storage",
        "disk storage for this node (x-pve volumes without one, and `new`)",
    ),
    (
        "template-storage",
        "storage holding the base template (cluster-wide)",
    ),
    (
        "template-base",
        "upstream template name prefix (cluster-wide)",
    ),
    ("owner", "default uid:gid for volume directories"),
    ("bridge", "bridge for `new`"),
    ("cores", "cores for `new`"),
    ("memory", "memory in MiB for `new`"),
    ("rootfs-size", "rootfs size for `new`"),
];

pub fn show(ctx: &Ctx) -> Result<()> {
    let c = &ctx.config;
    let d = Config::default();
    let row = |key: &str, value: String, source: &str| {
        println!("{key:<17} {value:<24} {source}");
    };
    println!("KEY               VALUE                    SOURCE");
    match c.storage_for(&ctx.node) {
        Some(s) => row("storage", s, &format!("node {}", ctx.node)),
        None => row(
            "storage",
            "-".into(),
            "unset: `new` asks, x-pve disks need a storage",
        ),
    }
    match &c.template.storage {
        Some(s) => row("template-storage", s.clone(), "cluster"),
        None => row("template-storage", "-".into(), "unset: `new` asks"),
    }
    let src = |same: bool| if same { "default" } else { "cluster" };
    row(
        "template-base",
        c.template.base.clone(),
        src(c.template.base == d.template.base),
    );
    row(
        "owner",
        c.defaults.owner.clone(),
        src(c.defaults.owner == d.defaults.owner),
    );
    row(
        "bridge",
        c.defaults.bridge.clone(),
        src(c.defaults.bridge == d.defaults.bridge),
    );
    row(
        "cores",
        c.defaults.cores.to_string(),
        src(c.defaults.cores == d.defaults.cores),
    );
    row(
        "memory",
        c.defaults.memory.to_string(),
        src(c.defaults.memory == d.defaults.memory),
    );
    row(
        "rootfs-size",
        c.defaults.rootfs_size.to_string(),
        src(c.defaults.rootfs_size == d.defaults.rootfs_size),
    );
    println!("\nfile: {}", config::PATH);
    Ok(())
}

pub fn set(ctx: &Ctx, key: &str, value: &str) -> Result<()> {
    let mut c = Config::load()?;
    match key {
        "storage" => c.set_storage_for(&ctx.node, Some(value.to_string())),
        "template-storage" => c.template.storage = Some(value.to_string()),
        "template-base" => c.template.base = value.to_string(),
        "owner" => {
            crate::spec::split_owner(value)?;
            c.defaults.owner = value.to_string();
        }
        "bridge" => c.defaults.bridge = value.to_string(),
        "cores" => c.defaults.cores = value.parse()?,
        "memory" => c.defaults.memory = value.parse()?,
        "rootfs-size" => c.defaults.rootfs_size = value.parse()?,
        _ => bail!("unknown key '{key}'; one of: {}", keys()),
    }
    c.save()?;
    eprintln!(
        "{key} = {value} ({})",
        if key == "storage" {
            ctx.node.as_str()
        } else {
            "cluster"
        }
    );
    Ok(())
}

pub fn unset(ctx: &Ctx, key: &str) -> Result<()> {
    let mut c = Config::load()?;
    let d = Config::default();
    match key {
        "storage" => c.set_storage_for(&ctx.node, None),
        "template-storage" => c.template.storage = None,
        "template-base" => c.template.base = d.template.base,
        "owner" => c.defaults.owner = d.defaults.owner,
        "bridge" => c.defaults.bridge = d.defaults.bridge,
        "cores" => c.defaults.cores = d.defaults.cores,
        "memory" => c.defaults.memory = d.defaults.memory,
        "rootfs-size" => c.defaults.rootfs_size = d.defaults.rootfs_size,
        _ => bail!("unknown key '{key}'; one of: {}", keys()),
    }
    c.save()?;
    eprintln!("{key} unset");
    Ok(())
}

pub fn keys() -> String {
    KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
}

pub fn help() -> String {
    KEYS.iter()
        .map(|(k, d)| format!("  {k:<17} {d}"))
        .collect::<Vec<_>>()
        .join("\n")
}
