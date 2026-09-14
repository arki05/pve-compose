//! `upgrade`: newer images. `docker upgrade` is `compose pull` then the
//! docker half of apply; the bare `upgrade` runs the pct half first so a
//! volume added to the document lands before the stack restarts. There is
//! no `pct upgrade`: the wrapper's base does not change (docs/ROADMAP.md).

use anyhow::Result;

use crate::ops::{apply, Ctx};

pub fn docker_upgrade(ctx: &Ctx, vmid: u32, o: apply::Options) -> Result<()> {
    apply::docker_apply(ctx, vmid, o)
}

pub fn upgrade(ctx: &Ctx, vmid: u32, o: apply::Options) -> Result<()> {
    apply::apply(ctx, vmid, o)
}
