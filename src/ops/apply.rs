//! `apply`: make the wrapper and the stack match the document.
//!
//! `pct apply` runs the plan (disks, features, tag). `docker apply` renders
//! the compose file, pushes it, brings the stack up, and writes the facts.
//! The bare `apply` is both, pct first, because the volumes have to be
//! mounted before compose binds them.
//!
//! A changed `features` line only takes effect on restart, and docker cannot
//! start without `nesting`. So when the plan changed features, `apply` stops
//! after the pct half and says so, unless asked to reboot the guest itself.

use anyhow::{bail, Result};

use crate::lock::GuestLock;
use crate::ops::{self, provision, Ctx, Guest};
use crate::pct;
use crate::plan::{self, Op};
use crate::spec::{self, Vars};
use crate::stack;
use crate::STACK_DIR;

/// How `apply` and `upgrade` behave beyond the default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// `docker compose pull` before `up`.
    pub pull: bool,
    /// Reboot the guest when the plan changed its features, then continue.
    pub reboot: bool,
    /// Render and push the compose file, write the facts, but never
    /// `compose up`.
    pub no_up: bool,
    /// `compose up` only if the stack is already up (some container of it
    /// running); otherwise behave as `no_up`. The loop's mode: a document
    /// change lands in the file, and starting the stack stays a human's act.
    pub only_if_up: bool,
}

/// Builds the pct-level plan for a loaded guest.
pub fn pct_plan(ctx: &Ctx, g: &Guest) -> Result<plan::Plan> {
    plan::build(&plan::Inputs {
        node: &ctx.node,
        config: &ctx.config,
        tag: ctx.tag.as_deref(),
        guest: &g.config,
        doc: &g.read.doc,
        volumes: &g.volumes,
    })
}

/// The pct half, under the guest's lock.
pub fn pct_apply(ctx: &Ctx, vmid: u32, o: Options) -> Result<usize> {
    let _lock = GuestLock::take(vmid, 60)?;
    ops::require_running(ctx, vmid)?;
    let g = ops::load(ctx, vmid)?;
    pct_apply_guest(ctx, &g, o)
}

/// The pct half for a loaded guest whose lock the caller holds. Returns the
/// number of operations run.
pub fn pct_apply_guest(ctx: &Ctx, g: &Guest, o: Options) -> Result<usize> {
    let vmid = g.vmid();
    let p = pct_plan(ctx, g)?;
    for n in &p.notes {
        eprintln!("note: {n}");
    }
    if p.is_empty() {
        eprintln!("pct: nothing to do");
        return Ok(0);
    }
    eprintln!("pct: applying {} change(s) to {vmid}", p.ops.len());
    // Features are the first op, so they are set even when a later op
    // fails; the restart they need must be said either way, or the next
    // plan sees them already set and the message is lost for good.
    let features_changed = p.ops.iter().any(|op| matches!(op, Op::SetFeatures(_)));
    let executed = plan::execute(&g.config, &p);
    if features_changed {
        if let Err(e) = &executed {
            eprintln!("pct: features were changed before this failed and take effect on restart; `pct reboot {vmid}` before the next apply ({e})");
        }
    }
    executed?;
    if features_changed {
        if o.reboot {
            eprintln!("pct: features changed; rebooting {vmid}");
            pct::reboot(vmid)?;
            pct::wait_ready(vmid, 90)?;
        } else {
            bail!(
                "guest {vmid}: features changed and take effect on restart; `pct reboot {vmid}`, then apply again (or apply --reboot)"
            );
        }
    }
    Ok(p.ops.len())
}

/// The substitution values for a guest.
pub fn vars(ctx: &Ctx, g: &Guest) -> Result<Vars> {
    let (uid, gid) = spec::split_owner(&g.owner(ctx))?;
    Ok(Vars {
        vmid: g.vmid(),
        uid,
        gid,
        name: g.config.hostname.clone().unwrap_or_default(),
        project: g.project(),
    })
}

/// The compose text the guest should have.
pub fn render(ctx: &Ctx, g: &Guest) -> Result<String> {
    spec::render(&g.read.doc.spec, &g.volumes, &vars(ctx, g)?)
}

/// The docker half, under the guest's lock.
pub fn docker_apply(ctx: &Ctx, vmid: u32, o: Options) -> Result<()> {
    let _lock = GuestLock::take(vmid, 60)?;
    ops::require_running(ctx, vmid)?;
    let g = ops::load(ctx, vmid)?;
    docker_apply_guest(ctx, &g, o).map(|_| ())
}

/// The docker half for a loaded guest whose lock the caller holds. The
/// config must be current: after a pct half, refresh it. Returns whether
/// the stack was brought up.
pub fn docker_apply_guest(ctx: &Ctx, g: &Guest, o: Options) -> Result<bool> {
    let vmid = g.vmid();
    if g.config.mount_at(STACK_DIR).is_none() {
        bail!("guest {vmid}: no stack disk at {STACK_DIR} yet; run `pve-compose pct apply {vmid}` first");
    }
    if !stack::stack_is_mounted(vmid)? {
        bail!("guest {vmid}: {STACK_DIR} is not mounted inside the guest");
    }
    provision::ensure_docker(vmid)?;

    let default_owner = g.owner(ctx);
    let owners: Vec<(String, String)> = g
        .volumes
        .iter()
        .map(|v| {
            (
                v.dir(),
                v.owner.clone().unwrap_or_else(|| default_owner.clone()),
            )
        })
        .collect();
    stack::ensure_layout(vmid, &owners)?;

    let text = render(ctx, g)?;
    let current = stack::read_compose(vmid)?;
    // Up means: a compose file was there and one of its containers runs.
    // Decided before the file is rewritten, since `ps` reads the file.
    let has_services = g
        .read
        .doc
        .spec
        .get("services")
        .and_then(|v| v.as_mapping())
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    let start = if o.no_up || !has_services {
        false
    } else if o.only_if_up {
        current.is_some() && stack::ps(vmid)?.iter().any(|c| c.state == "running")
    } else {
        true
    };
    if current.as_deref() != Some(text.as_str()) {
        eprintln!("docker: writing {}", stack::COMPOSE_FILE);
        stack::write_compose(vmid, &text)?;
    }
    if start {
        if o.pull {
            eprintln!("docker: pull");
            stack::pull(vmid)?;
        }
        eprintln!("docker: up");
        stack::up(vmid)?;
    } else if !has_services {
        eprintln!("docker: file written; no services in the document, nothing to start");
    } else {
        eprintln!("docker: file written, stack not started; `pve-compose docker apply {vmid}` brings it up");
    }
    let facts = stack::facts_now(&g.read.digest, stack::read_facts(vmid)?);
    stack::write_facts(vmid, &facts)?;
    Ok(start)
}

/// Both halves, pct first, under one lock.
pub fn apply(ctx: &Ctx, vmid: u32, o: Options) -> Result<()> {
    let _lock = GuestLock::take(vmid, 60)?;
    apply_locked(ctx, vmid, o)
}

/// [`apply`] for a caller that already holds the guest's lock. The config is
/// read again after a pct half that changed something: the docker half must
/// see the mounts it created.
pub fn apply_locked(ctx: &Ctx, vmid: u32, o: Options) -> Result<()> {
    ops::require_running(ctx, vmid)?;
    let g = ops::load(ctx, vmid)?;
    let changed = pct_apply_guest(ctx, &g, o)?;
    let g = if changed > 0 {
        ops::refresh_config(ctx, g)?
    } else {
        g
    };
    docker_apply_guest(ctx, &g, o).map(|_| ())
}
