//! `status`: every compose guest on this node, or one, as a table or JSON.

use anyhow::Result;
use serde::Serialize;

use crate::doc;
use crate::ops::{self, apply, Ctx};
use crate::pct;
use crate::stack;
use crate::STACK_DIR;

#[derive(Debug, Serialize)]
pub struct Row {
    pub vmid: u32,
    pub name: String,
    pub node: String,
    pub running: bool,
    pub policy: Option<String>,
    /// `applied`, `down` (file applied, no container running), `pending`,
    /// `never`, `stopped`, `no document`, `error: ...`
    pub state: String,
    pub applied_at: Option<String>,
    pub template: Option<String>,
    pub pct_pending: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<ContainerRow>,
}

#[derive(Debug, Serialize)]
pub struct ContainerRow {
    pub service: String,
    pub name: String,
    pub state: String,
    pub status: String,
}

/// Which level to report. `Both` adds the container list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pct,
    Docker,
    Both,
}

fn mode(m: doc::Mode) -> &'static str {
    match m {
        doc::Mode::Auto => "auto",
        doc::Mode::Manual => "manual",
    }
}

/// One row, given the document already read (or not there).
fn row(ctx: &Ctx, l: &pct::Listed, read: Option<doc::Read>, level: Level) -> Row {
    let mut r = Row {
        vmid: l.vmid,
        name: l.name.clone(),
        node: ctx.node.clone(),
        running: l.running,
        policy: None,
        state: String::new(),
        applied_at: None,
        template: None,
        pct_pending: None,
        containers: Vec::new(),
    };
    let Some(read) = read else {
        r.state = "no document".into();
        return r;
    };
    r.policy = Some(format!(
        "pct={} docker={} pull={}",
        mode(read.doc.policy.pct),
        mode(read.doc.policy.docker),
        mode(read.doc.policy.pull)
    ));
    if !l.running {
        r.state = "stopped".into();
        return r;
    }
    let digest = read.digest.clone();
    let g = match ops::load_with(ctx, read) {
        Ok(g) => g,
        Err(e) => {
            r.state = format!("error: {e}");
            return r;
        }
    };
    if level != Level::Docker {
        match apply::pct_plan(ctx, &g) {
            Ok(p) => r.pct_pending = Some(p.ops.len()),
            Err(e) => {
                r.state = format!("error: {e}");
                return r;
            }
        }
    }
    if level == Level::Pct {
        r.state = if r.pct_pending == Some(0) {
            "applied"
        } else {
            "pending"
        }
        .into();
        return r;
    }
    if g.config.mount_at(STACK_DIR).is_none() {
        r.state = "never".into();
        return r;
    }
    match stack::read_facts(l.vmid) {
        Ok(Some(f)) => {
            r.applied_at = Some(f.applied_at.clone());
            r.template = f.template.clone();
            r.state = if f.digest == digest {
                "applied"
            } else {
                "pending"
            }
            .into();
        }
        Ok(None) => r.state = "never".into(),
        Err(e) => {
            r.state = format!("error: {e}");
            return r;
        }
    }
    if r.state == "applied" && r.pct_pending.unwrap_or(0) > 0 {
        r.state = "pending".into();
    }
    // Docker and Both both know whether the stack is up; only Both lists it.
    if let Ok(list) = stack::ps(l.vmid) {
        let running = list.iter().any(|c| c.state == "running");
        if level == Level::Both {
            r.containers = list
                .into_iter()
                .map(|c| ContainerRow {
                    service: c.service,
                    name: c.name,
                    state: c.state,
                    status: c.status,
                })
                .collect();
        }
        if r.state == "applied" && !running {
            r.state = "down".into();
        }
    }
    r
}

/// Rows for the guests asked for: one vmid, or every selected guest on this
/// node, plus any guest with a document but without the tag, so a forgotten
/// tag shows. Each document is read once.
pub fn rows(ctx: &Ctx, vmid: Option<u32>, level: Level) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    for l in pct::list(&ctx.node)? {
        if l.is_template {
            continue;
        }
        if let Some(v) = vmid {
            if l.vmid != v {
                continue;
            }
        }
        let read = match doc::read(l.vmid) {
            Ok(r) => r,
            Err(e) => {
                out.push(Row {
                    vmid: l.vmid,
                    name: l.name.clone(),
                    node: ctx.node.clone(),
                    running: l.running,
                    policy: None,
                    state: format!("error: {e}"),
                    applied_at: None,
                    template: None,
                    pct_pending: None,
                    containers: Vec::new(),
                });
                continue;
            }
        };
        if vmid.is_none() && read.is_none() && !ctx.selected(&l.tags) {
            continue;
        }
        out.push(row(ctx, &l, read, level));
    }
    Ok(out)
}

pub fn print_table(rows: &[Row], level: Level) {
    println!("VMID    NAME                 STATE    APPLIED    PCT      POLICY");
    for r in rows {
        println!(
            "{:<7} {:<20} {:<8} {:<10} {:<8} {}",
            r.vmid,
            trunc(&r.name, 20),
            trunc(&r.state, 8),
            r.applied_at
                .as_deref()
                .map(|s| s.get(..10).unwrap_or(s).to_string())
                .unwrap_or_else(|| "-".into()),
            match r.pct_pending {
                Some(0) => "ok".to_string(),
                Some(n) => format!("{n} pending"),
                None => "-".to_string(),
            },
            r.policy.as_deref().unwrap_or("-")
        );
        if level == Level::Both {
            for c in &r.containers {
                println!(
                    "        {:<20} {:<8} {}",
                    trunc(&c.service, 20),
                    trunc(&c.state, 8),
                    c.status
                );
            }
        }
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
