//! Running the node's own tools: `pct`, `pvesh`, `pve-meta`, `pveam`.
//!
//! Two shapes: `run` captures output and fails on a non-zero exit with the
//! command line and stderr in the error, for anything whose output the tool
//! reads; `stream` inherits the terminal, for anything a human wants to watch
//! (compose output, a template build). Both log the command at debug level.
//!
//! Every call has a deadline. The loop is single-threaded and most of what it
//! runs ends up inside a guest, so one hung `docker info` or `apt-get` in one
//! container must not hold the node's other guests for good: `run` starts
//! the command in a process group of its own, kills the whole group past the
//! deadline, and caps what it reads ([`MAX_OUTPUT`], [`STDERR_CAP`]); `stream`
//! does the same killing around an inherited terminal. [`TIMEOUT`] covers reads, probes and PVE's
//! own quick verbs; [`LONG_TIMEOUT`] is for the ones that legitimately take
//! minutes (a compose up that pulls, the docker install, a template
//! download, a disk allocation) and is named at those call sites.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// The deadline for a read, a probe or one of PVE's quick verbs.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The deadline for the slow ones, asked for explicitly.
pub const LONG_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The most stdout a captured command may produce.
pub const MAX_OUTPUT: usize = 4 * 1024 * 1024;

/// The most stderr a captured command may produce.
pub const STDERR_CAP: usize = 64 * 1024;

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
    let mut child = {
        use std::os::unix::process::CommandExt as _;
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot run {program}"))?
    };
    let over = Arc::new(AtomicBool::new(false));
    let out = capped(
        child.stdout.take().expect("piped"),
        MAX_OUTPUT,
        over.clone(),
    );
    let err = capped(
        child.stderr.take().expect("piped"),
        STDERR_CAP,
        over.clone(),
    );
    let what = format!("{program} {}", args.first().unwrap_or(&""));
    let pgid = child.id() as i32;
    // SAFETY: kill(2) on the negated id of the group the child leads.
    let kill = || unsafe { libc::kill(-pgid, libc::SIGKILL) };
    let deadline = Instant::now() + timeout;
    let mut status = None;
    // Done when the command has exited and both pipes are at EOF: something
    // it left behind in its group still holding a pipe counts as not done,
    // so it is under the same deadline.
    loop {
        if over.load(Ordering::Relaxed) {
            kill();
            let _ = child.wait();
            bail!("{what}: more output than allowed, killed");
        }
        if status.is_none() {
            status = child.try_wait()?;
        }
        if status.is_some() && out.is_finished() && err.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            kill();
            let _ = child.wait();
            bail!("{what}: no answer after {}s, killed", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if over.load(Ordering::Relaxed) {
        bail!("{what}: more output than allowed");
    }
    Ok(Output {
        status: status.and_then(|s| s.code()).unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
    })
}

/// Reads `r` to its end on a thread of its own, up to `cap` bytes; past it,
/// stops reading and raises `over`, which gets the command killed.
fn capped<R: Read + Send + 'static>(
    mut r: R,
    cap: usize,
    over: Arc<AtomicBool>,
) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) if buf.len() + n > cap => {
                    over.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        buf
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
    fn what_it_leaves_behind_holding_a_pipe_is_killed_too() {
        let started = Instant::now();
        let err = run_status_within(
            "sh",
            &["-c", "sleep 30 & echo started"],
            Duration::from_millis(300),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no answer after 0s, killed"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_is_capped() {
        let started = Instant::now();
        let err = run_status_within("sh", &["-c", "yes"], Duration::from_secs(30))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more output than allowed"), "{err}");
        let err = run_status_within("sh", &["-c", "yes >&2"], Duration::from_secs(30))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more output than allowed"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
        // Right at the cap is fine.
        let out = run_status("sh", &["-c", &format!("head -c {MAX_OUTPUT} /dev/zero")]).unwrap();
        assert_eq!(out.stdout.len(), MAX_OUTPUT);
    }

    #[test]
    fn a_missing_program_is_an_error_naming_it() {
        let err = run_status("/nonexistent/pve-compose-test", &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cannot run /nonexistent/pve-compose-test"),
            "{err}"
        );
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
