//! The CLI. `pve-compose [pct|docker] <verb> <vmid>`: `pct` verbs act on the
//! wrapper through PVE, `docker` verbs act inside it, and a bare verb does
//! both, pct first. Unknown `docker` verbs pass through to `docker compose`
//! in the stack directory.
//!
//! A vmid on another node is forwarded: the same command line is run there
//! over the cluster's own root SSH, so every verb works from any node while
//! the per-guest lock is always taken on the owning node.

use std::process::ExitCode;

use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};

use pve_compose::cmd;
use pve_compose::lock::GuestLock;
use pve_compose::ops::{self, apply, configcmd, daemon, diff, new, status, upgrade, Ctx};
use pve_compose::pct;
use pve_compose::size::Size;
use pve_compose::stack;

#[derive(Parser)]
#[command(
    name = "pve-compose",
    version,
    about = "docker compose stacks in LXC wrappers, from pve-meta documents"
)]
struct Cli {
    /// Echo every command run.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Reconcile both levels: the wrapper, then the stack.
    Apply(ApplyArgs),
    /// Show what apply would change, at both levels.
    Diff(Vmid),
    /// Compose guests on this node, or one guest with its containers.
    Status(StatusArgs),
    /// Pull newer images, then apply both levels.
    Upgrade(UpgradeArgs),
    /// A new wrapper from the template with an empty document.
    New(Box<NewArgs>),
    /// Persisted choices: show, set or unset one (storage is per node).
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },
    /// The wrapper, through PVE.
    Pct {
        #[command(subcommand)]
        cmd: PctCmd,
    },
    /// The stack, inside the wrapper.
    Docker {
        #[command(subcommand)]
        cmd: DockerCmd,
    },
    /// The reconcile loop (run by the systemd unit).
    Daemon,
}

#[derive(Subcommand)]
enum PctCmd {
    /// Disks, features and tag as the document asks.
    Apply(ApplyArgs),
    Diff(Vmid),
    Status(StatusArgs),
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Set a key; storage applies to this node, the rest to the cluster.
    Set {
        #[arg(
            help = "storage, template-storage, template-base, owner, bridge, cores, memory, rootfs-size"
        )]
        key: String,
        value: String,
    },
    /// Back to the default (storage: unset for this node).
    Unset { key: String },
}

#[derive(Subcommand)]
enum DockerCmd {
    /// Render and push compose.yaml, then `compose up -d`.
    Apply(ApplyArgs),
    Diff(Vmid),
    Status(StatusArgs),
    /// `compose pull`, then apply.
    Upgrade(UpgradeArgs),
    /// Anything else: passed to `docker compose` in the stack directory.
    #[command(external_subcommand)]
    Compose(Vec<String>),
}

#[derive(Args)]
struct Vmid {
    vmid: u32,
}

#[derive(Args)]
struct ApplyArgs {
    vmid: u32,
    /// Render and push the compose file, but do not `compose up`.
    #[arg(long)]
    no_up: bool,
}

impl ApplyArgs {
    fn options(&self) -> apply::Options {
        apply::Options {
            pull: false,
            no_up: self.no_up,
            only_if_up: false,
        }
    }
}

#[derive(Args)]
struct UpgradeArgs {
    vmid: u32,
}

impl UpgradeArgs {
    fn options(&self) -> apply::Options {
        apply::Options {
            pull: true,
            no_up: false,
            only_if_up: false,
        }
    }
}

#[derive(Args)]
struct StatusArgs {
    vmid: Option<u32>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct NewArgs {
    vmid: u32,
    /// Hostname; also the compose project name, lowercased.
    #[arg(long)]
    name: String,
    #[arg(long)]
    template: Option<String>,
    /// Storage for the rootfs (default: this node's stored choice, else asked).
    #[arg(long)]
    storage: Option<String>,
    #[arg(long)]
    rootfs_size: Option<Size>,
    #[arg(long)]
    bridge: Option<String>,
    /// `dhcp` or a CIDR.
    #[arg(long)]
    ip: Option<String>,
    #[arg(long)]
    gateway: Option<String>,
    #[arg(long)]
    cores: Option<u32>,
    /// MiB.
    #[arg(long)]
    memory: Option<u64>,
    /// Set everything up but do not `compose up`: for migrating data in first.
    #[arg(long)]
    no_up: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    cmd::set_verbose(cli.verbose);
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pve-compose: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The vmid a command is about, if it is about one.
fn target(cmd: &Cmd) -> Option<u32> {
    match cmd {
        Cmd::Apply(a) => Some(a.vmid),
        Cmd::Upgrade(u) => Some(u.vmid),
        Cmd::Diff(v) => Some(v.vmid),
        Cmd::Status(s) => s.vmid,
        Cmd::New(_) | Cmd::Daemon | Cmd::Config { .. } => None,
        Cmd::Pct { cmd } => match cmd {
            PctCmd::Apply(a) => Some(a.vmid),
            PctCmd::Diff(v) => Some(v.vmid),
            PctCmd::Status(s) => s.vmid,
        },
        Cmd::Docker { cmd } => match cmd {
            DockerCmd::Apply(a) => Some(a.vmid),
            DockerCmd::Upgrade(u) => Some(u.vmid),
            DockerCmd::Diff(v) => Some(v.vmid),
            DockerCmd::Status(s) => s.vmid,
            DockerCmd::Compose(args) => args.get(1).and_then(|s| s.parse().ok()),
        },
    }
}

/// Forwards the command line to the node owning `vmid` if that is not this
/// node. Returns `true` when it was forwarded (and has run there).
fn forward_if_remote(ctx: &Ctx, vmid: u32) -> Result<bool> {
    let Some((node, kind)) = pct::owner_node(vmid)? else {
        bail!("no guest {vmid} in the cluster");
    };
    if kind != "lxc" {
        bail!("guest {vmid} is not a container");
    }
    if node == ctx.node {
        return Ok(false);
    }
    // PVE's own inter-node command: the right address, host-key alias and
    // known-hosts file, exactly as a migration uses them.
    let out = cmd::run(
        "perl",
        &[
            "-MPVE::SSHInfo",
            "-e",
            "print join(\"\\0\", @{PVE::SSHInfo::ssh_info_to_command(PVE::SSHInfo::get_ssh_info($ARGV[0]))})",
            &node,
        ],
    )?;
    let ssh: Vec<&str> = out.stdout.split('\0').filter(|s| !s.is_empty()).collect();
    if ssh.is_empty() {
        bail!("cannot build the ssh command for node {node}");
    }
    eprintln!("guest {vmid} is on {node}; running there");
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let quoted: Vec<String> = argv.iter().map(|a| stack::shell_quote(a)).collect();
    let remote = format!("pve-compose {}", quoted.join(" "));
    let mut args: Vec<&str> = ssh[1..].to_vec();
    args.push("-t");
    args.push(&remote);
    cmd::stream_tty(ssh[0], &args)?;
    Ok(true)
}

fn run(cli: Cli) -> Result<()> {
    let ctx = Ctx::load()?;
    if let Some(vmid) = target(&cli.cmd) {
        if forward_if_remote(&ctx, vmid)? {
            return Ok(());
        }
    }
    match cli.cmd {
        Cmd::Apply(a) => apply::apply(&ctx, a.vmid, a.options()),
        Cmd::Diff(v) => diff::diff(&ctx, v.vmid),
        Cmd::Status(s) => show_status(&ctx, s, status::Level::Both),
        Cmd::Upgrade(a) => upgrade::upgrade(&ctx, a.vmid, a.options()),
        Cmd::New(a) => new::new(
            &ctx,
            &new::NewArgs {
                vmid: a.vmid,
                name: a.name,
                template: a.template,
                storage: a.storage,
                rootfs_size: a.rootfs_size,
                bridge: a.bridge,
                ip: a.ip,
                gateway: a.gateway,
                cores: a.cores,
                memory: a.memory,
                no_up: a.no_up,
            },
        ),
        Cmd::Daemon => daemon::run(ctx),
        Cmd::Config { cmd } => match cmd {
            None => configcmd::show(&ctx),
            Some(ConfigCmd::Set { key, value }) => configcmd::set(&ctx, &key, &value),
            Some(ConfigCmd::Unset { key }) => configcmd::unset(&ctx, &key),
        },
        Cmd::Pct { cmd } => match cmd {
            PctCmd::Apply(a) => apply::pct_apply(&ctx, a.vmid).map(|_| ()),
            PctCmd::Diff(v) => diff::pct_diff(&ctx, v.vmid),
            PctCmd::Status(s) => show_status(&ctx, s, status::Level::Pct),
        },
        Cmd::Docker { cmd } => match cmd {
            DockerCmd::Apply(a) => apply::docker_apply(&ctx, a.vmid, a.options()),
            DockerCmd::Diff(v) => diff::docker_diff(&ctx, v.vmid),
            DockerCmd::Status(s) => show_status(&ctx, s, status::Level::Docker),
            DockerCmd::Upgrade(a) => upgrade::docker_upgrade(&ctx, a.vmid, a.options()),
            DockerCmd::Compose(args) => {
                // `pve-compose docker <verb> <vmid> [args...]`
                let verb = args.first().cloned().unwrap_or_default();
                let vmid: u32 = match args.get(1).and_then(|s| s.parse().ok()) {
                    Some(v) => v,
                    None => bail!("usage: pve-compose docker {verb} <vmid> [compose arguments]"),
                };
                // The same lock every other verb takes: a passthrough is a
                // `docker compose` in the stack directory like the loop's,
                // and the two must not run there at once.
                let _lock = GuestLock::take(vmid, 60)?;
                ops::require_running(&ctx, vmid)?;
                let mut rest = vec![verb];
                rest.extend(args.iter().skip(2).cloned());
                stack::passthrough(vmid, &rest)
            }
        },
    }
}

fn show_status(ctx: &Ctx, s: StatusArgs, level: status::Level) -> Result<()> {
    let rows = status::rows(ctx, s.vmid, level)?;
    if s.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("no compose guests on {}", ctx.node);
    } else {
        status::print_table(&rows, level);
    }
    Ok(())
}
