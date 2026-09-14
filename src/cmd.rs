//! Running the node's own tools: `pct`, `pvesh`, `pve-meta`, `pveam`.
//!
//! Two shapes: `run` captures output and fails on a non-zero exit with the
//! command line and stderr in the error, for anything whose output the tool
//! reads; `stream` inherits the terminal, for anything a human wants to watch
//! (compose output, a template build). Both log the command at debug level.

use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

/// Captured output of a finished command.
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

fn render(program: &str, args: &[&str]) -> String {
    let mut s = String::from(program);
    for a in args {
        s.push(' ');
        if a.contains(' ') || a.is_empty() {
            s.push('\'');
            s.push_str(a);
            s.push('\'');
        } else {
            s.push_str(a);
        }
    }
    s
}

/// Runs `program args`, capturing both streams. Fails on a non-zero exit.
pub fn run(program: &str, args: &[&str]) -> Result<Output> {
    let out = run_status(program, args)?;
    if out.status != 0 {
        bail!(
            "{} failed (exit {}){}",
            render(program, args),
            out.status,
            if out.stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", out.stderr.trim())
            }
        );
    }
    Ok(out)
}

/// Like [`run`], but a non-zero exit is returned, not an error. For commands
/// whose exit status carries meaning (`pve-meta get` exits 2 for "not there").
pub fn run_status(program: &str, args: &[&str]) -> Result<Output> {
    if verbose() {
        eprintln!("+ {}", render(program, args));
    }
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {program}"))?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Runs `program args` with the terminal attached, so the user sees the
/// output as it happens. Fails on a non-zero exit.
pub fn stream(program: &str, args: &[&str]) -> Result<()> {
    if verbose() {
        eprintln!("+ {}", render(program, args));
    }
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    if !status.success() {
        return Err(anyhow!(
            "{} failed (exit {})",
            render(program, args),
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Like [`stream`], with stdin attached too: for an ssh to another node
/// whose command may ask a question.
pub fn stream_tty(program: &str, args: &[&str]) -> Result<()> {
    if verbose() {
        eprintln!("+ {}", render(program, args));
    }
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    if !status.success() {
        return Err(anyhow!(
            "{} failed (exit {})",
            render(program, args),
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Runs `program args` and parses its stdout as JSON.
pub fn json(program: &str, args: &[&str]) -> Result<serde_json::Value> {
    let out = run(program, args)?;
    serde_json::from_str(&out.stdout)
        .with_context(|| format!("{}: not JSON: {}", render(program, args), out.stdout.trim()))
}

/// `pvesh get <path> [args] --output-format json`.
pub fn pvesh_get(path: &str, args: &[&str]) -> Result<serde_json::Value> {
    let mut all = vec!["get", path];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--output-format", "json"]);
    json("pvesh", &all)
}

static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turns on command echoing to stderr (`-v`).
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The local node's name, as PVE knows it: where `/etc/pve/local` points
/// (`nodes/<name>`), or the hostname up to the first dot without pmxcfs.
pub fn nodename() -> Result<String> {
    if let Ok(target) = std::fs::read_link("/etc/pve/local") {
        if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
    }
    let out = run("hostname", &[])?;
    let h = out.stdout.trim();
    let short = h.split('.').next().unwrap_or(h);
    if short.is_empty() {
        bail!("cannot determine the node name");
    }
    Ok(short.to_string())
}
