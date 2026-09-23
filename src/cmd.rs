//! Running the node's own tools: `pct`, `pvesh`, `pve-meta`, `pveam`.
//!
//! Two shapes: `run` captures output and fails on a non-zero exit with the
//! command line and stderr in the error, for anything whose output the tool
//! reads; `stream` inherits the terminal, for anything a human wants to watch
//! (compose output, a template build). Both log the command at debug level.
//!
//! Every call has a deadline. The loop is single-threaded and most of what it
//! runs ends up inside a guest, so one hung `docker info` or `apt-get` in one
//! container must not hold the node's other guests for good: `run` goes
//! through [`pve_meta_guest_files::pct::run`], which kills the process group
//! past the deadline and caps the output, and `stream` does the same killing
//! around an inherited terminal. [`TIMEOUT`] covers reads, probes and PVE's
//! own quick verbs; [`LONG_TIMEOUT`] is for the ones that legitimately take
//! minutes (a compose up that pulls, the docker install, a template
//! download, a disk allocation) and is named at those call sites.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// The deadline for a read, a probe or one of PVE's quick verbs.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The deadline for the slow ones, asked for explicitly.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Captured output of a finished command.
#[derive(Debug)]
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
    run_within(program, args, TIMEOUT)
}

/// [`run`] with a deadline of its own.
pub fn run_within(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
    let out = run_status_within(program, args, timeout)?;
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
    run_status_within(program, args, TIMEOUT)
}

/// [`run_status`] with a deadline of its own. Past it the command's whole
/// process group is killed and this is an error saying so.
pub fn run_status_within(program: &str, args: &[&str], timeout: Duration) -> Result<Output> {
    if verbose() {
        eprintln!("+ {}", render(program, args));
    }
    let out = pve_meta_guest_files::pct::run(program, args, None, timeout)?;
    Ok(Output {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: out.stderr,
    })
}

/// Runs `program args` with the terminal attached, so the user sees the
/// output as it happens, with no deadline: for a command a human started and
/// is watching (`docker compose logs -f`), which only they know the length of.
/// Fails on a non-zero exit.
pub fn stream(program: &str, args: &[&str]) -> Result<()> {
    let mut child = spawn_streaming(program, args, false)?;
    finish(program, args, child.wait()?)
}

/// [`stream`] with a deadline: for the long ones the daemon runs too, where
/// nobody is watching and a hung command would hold every other guest on the
/// node. Without a terminal the command gets its own process group, so the
/// whole of it is killed; with one it stays in the shell's, so `^C` still
/// reaches it, and the deadline kills the command itself.
pub fn stream_within(program: &str, args: &[&str], timeout: Duration) -> Result<()> {
    let own_group = !crate::prompt::interactive();
    let mut child = spawn_streaming(program, args, own_group)?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            let pid = child.id() as i32;
            // SAFETY: kill(2) on the child, or on the negated id of the
            // process group it leads when it was given one.
            unsafe { libc::kill(if own_group { -pid } else { pid }, libc::SIGKILL) };
            let _ = child.wait();
            bail!(
                "{}: no answer after {}s, killed",
                render(program, args),
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    finish(program, args, status)
}

fn spawn_streaming(program: &str, args: &[&str], own_group: bool) -> Result<std::process::Child> {
    if verbose() {
        eprintln!("+ {}", render(program, args));
    }
    let mut c = Command::new(program);
    c.args(args).stdin(Stdio::null());
    if own_group {
        use std::os::unix::process::CommandExt as _;
        c.process_group(0);
    }
    c.spawn().with_context(|| format!("cannot run {program}"))
}

fn finish(program: &str, args: &[&str], status: std::process::ExitStatus) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_that_never_ends_is_killed() {
        let started = Instant::now();
        let out = run_status_within(
            "sh",
            &["-c", "sleep 30 & sleep 30"],
            Duration::from_millis(300),
        );
        let err = out.unwrap_err().to_string();
        assert!(err.contains("killed"), "{err}");
        let err = stream_within(
            "sh",
            &["-c", "sleep 30 & sleep 30"],
            Duration::from_millis(300),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no answer after 0s, killed"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_and_status_come_back() {
        let out = run_status("sh", &["-c", "echo out; echo err >&2; exit 3"]).unwrap();
        assert_eq!(
            (out.status, out.stdout.trim(), out.stderr.trim()),
            (3, "out", "err")
        );
        let err = run("sh", &["-c", "echo nope >&2; exit 1"])
            .unwrap_err()
            .to_string();
        assert!(err.contains("exit 1") && err.contains("nope"), "{err}");
        stream("sh", &["-c", "exit 0"]).unwrap();
    }
}
