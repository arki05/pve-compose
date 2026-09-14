//! `diff`: what `apply` would do, without doing it. The pct half prints the
//! plan; the docker half prints a line diff of the compose file the guest
//! has against the one the document renders to.

use anyhow::Result;

use crate::ops::{self, apply, Ctx, Guest};
use crate::stack;
use crate::STACK_DIR;

pub fn pct_diff(ctx: &Ctx, vmid: u32) -> Result<()> {
    pct_diff_guest(ctx, &ops::load(ctx, vmid)?)
}

pub fn pct_diff_guest(ctx: &Ctx, g: &Guest) -> Result<()> {
    let p = apply::pct_plan(ctx, g)?;
    for n in &p.notes {
        println!("note: {n}");
    }
    if p.is_empty() {
        println!("pct: up to date");
    } else {
        for op in &p.ops {
            println!("pct: {op}");
        }
    }
    Ok(())
}

pub fn docker_diff(ctx: &Ctx, vmid: u32) -> Result<()> {
    docker_diff_guest(ctx, &ops::load(ctx, vmid)?)
}

pub fn docker_diff_guest(ctx: &Ctx, g: &Guest) -> Result<()> {
    let vmid = g.vmid();
    if g.config.mount_at(STACK_DIR).is_none() {
        println!("docker: no stack disk yet (pct apply creates it)");
        return Ok(());
    }
    ops::require_running(ctx, vmid)?;
    let want = apply::render(ctx, g)?;
    let have = stack::read_compose(vmid)?.unwrap_or_default();
    match stack::read_facts(vmid)? {
        Some(f) if f.digest == g.read.digest => {
            println!(
                "docker: document unchanged since last apply ({})",
                f.applied_at
            )
        }
        Some(f) => println!(
            "docker: document changed since last apply ({})",
            f.applied_at
        ),
        None => println!("docker: never applied"),
    }
    if have != want {
        println!("docker: {} differs:", stack::COMPOSE_FILE);
        print!("{}", line_diff(&have, &want));
    } else {
        println!("docker: {} up to date", stack::COMPOSE_FILE);
    }
    Ok(())
}

pub fn diff(ctx: &Ctx, vmid: u32) -> Result<()> {
    let g = ops::load(ctx, vmid)?;
    pct_diff_guest(ctx, &g)?;
    docker_diff_guest(ctx, &g)
}

/// A plain line diff, `-`/`+`/` ` prefixed, by longest common subsequence.
/// Compose files are small, so the quadratic table is fine.
pub fn line_diff(a: &str, b: &str) -> String {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.push_str(&format!("  {}\n", a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push_str(&format!("- {}\n", a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+ {}\n", b[j]));
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_lines() {
        let d = line_diff("a\nb\nc\n", "a\nx\nc\n");
        assert_eq!(d, "  a\n- b\n+ x\n  c\n");
    }
}
