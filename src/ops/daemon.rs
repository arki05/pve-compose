//! The reconcile loop, one per node, acting only on guests whose config
//! lives on this node. PVE keeps exactly one owner per guest, so N nodes
//! running this never overlap, and a migration moves the responsibility.
//!
//! Level-triggered: poll pve-meta's version token; when it moves, walk the
//! running selected guests and apply whichever halves the document's policy
//! allows, skipping a guest whose document was already handled. Every
//! `full_every` seconds the memo is dropped and every guest is planned
//! again, which is what catches a change made outside the store.
//!
//! The docker half only ever applies a change to a stack that is up. On a
//! stack that is down, whether never started (`new --no-up`) or taken down
//! by hand, the loop renders and pushes the compose file so the pending
//! state lands, and does not start anything: starting is a human's act.
//!
//! The stored choices are re-read before every pass: a `config set storage`
//! on the node takes effect at the next poll, and a changed config drops the
//! memo so a guest refused for lack of a storage is planned again.
//!
//! Single-threaded on purpose: one guest at a time, in vmid order. A guest
//! whose apply is slow (a first docker install) delays the others on the
//! node for that long. Acceptable for the scale this is built for, and one
//! less thing that can interleave.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::cmd;
use crate::config::Config;
use crate::doc::{self, Mode};
use crate::notify;
use crate::ops::{self, apply, Ctx};
use crate::pct;
use crate::stack;

struct Memo {
    digest: String,
}

pub fn run(mut ctx: Ctx) -> Result<()> {
    let interval = Duration::from_secs(ctx.config.daemon.interval.max(5));
    let full_every = Duration::from_secs(ctx.config.daemon.full_every.max(60));
    let mut last_token = String::new();
    let mut last_full = Instant::now() - full_every;
    let mut memo: HashMap<u32, Memo> = HashMap::new();
    eprintln!(
        "pve-compose daemon on {}: polling every {}s, full pass every {}s",
        ctx.node,
        interval.as_secs(),
        full_every.as_secs()
    );
    notify::ready();
    loop {
        notify::watchdog();
        let token = match cmd::pvesh_get("/meta/version", &[]) {
            Ok(v) => v
                .get("token")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                eprintln!("version poll failed: {e}");
                std::thread::sleep(interval);
                continue;
            }
        };
        let full = last_full.elapsed() >= full_every;
        match Config::load() {
            Ok(c) if c != ctx.config => {
                eprintln!("configuration changed; planning every guest again");
                ctx.config = c;
                memo.clear();
            }
            Ok(_) => {}
            Err(e) => eprintln!("configuration not re-read: {e}"),
        }
        if token != last_token || full {
            if full {
                memo.clear();
                last_full = Instant::now();
            }
            if let Err(e) = pass(&ctx, &mut memo) {
                eprintln!("pass failed: {e}");
            }
            last_token = token;
        }
        std::thread::sleep(interval);
    }
}

fn pass(ctx: &Ctx, memo: &mut HashMap<u32, Memo>) -> Result<()> {
    for l in pct::list(&ctx.node)? {
        if l.is_template || !l.running || !ctx.selected(&l.tags) {
            continue;
        }
        let read = match doc::read(l.vmid) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("{}: {e}", l.vmid);
                continue;
            }
        };
        if memo
            .get(&l.vmid)
            .map(|m| m.digest == read.digest)
            .unwrap_or(false)
        {
            continue;
        }
        let digest = read.digest.clone();
        let policy = read.doc.policy.clone();
        let vmid = l.vmid;
        let result = (|| -> Result<()> {
            let _lock = crate::lock::GuestLock::take(vmid, 60)?;
            let mut g = ops::load_with(ctx, read)?;
            if policy.pct == Mode::Auto {
                let n = apply::pct_apply_guest(ctx, &g)?;
                if n > 0 {
                    eprintln!("{vmid}: pct applied {n} change(s)");
                    g = ops::refresh_config(ctx, g)?;
                }
            }
            // Facts are read only now: whoever held the lock before us (a
            // `new`, a hand-run apply) may have applied this very digest.
            let applied = stack::read_facts(vmid)?
                .map(|f| f.digest == g.read.digest)
                .unwrap_or(false);
            if policy.docker == Mode::Auto && !applied {
                let started = apply::docker_apply_guest(
                    ctx,
                    &g,
                    apply::Options {
                        pull: policy.pull == Mode::Auto,
                        no_up: policy.up == Mode::Manual,
                        only_if_up: true,
                    },
                )?;
                eprintln!(
                    "{vmid}: docker {} {}",
                    if started {
                        "applied"
                    } else if policy.up == Mode::Manual {
                        "rendered (up is manual, not started)"
                    } else {
                        "rendered (stack is down, not started)"
                    },
                    &g.read.digest[..12]
                );
            }
            Ok(())
        })();
        if let Err(e) = result {
            eprintln!("{vmid}: {e}");
        }
        // Remembered either way: a failure is retried on the next full pass
        // or the next document change, not every poll.
        memo.insert(vmid, Memo { digest });
        // One guest done is the progress the unit's WatchdogSec measures.
        notify::watchdog();
    }
    Ok(())
}
