//! pve-compose: a docker compose stack per LXC, described by the guest's
//! pve-meta document and reconciled from the node.
//!
//! The layering, top down:
//!
//! * `ops`: the verbs: new, apply, diff, status, upgrade, template, daemon. Each is a function over the types below.
//! * `plan`: the pct-level plan: what `pct set` calls turn the wrapper into
//!   what the document asks for. Pure over a `GuestConfig` and a `Doc`.
//! * `spec`: the compose document: its managed volumes, and the rendered
//!   `compose.yaml` that goes into the guest.
//! * `stack`: everything done inside the guest: the stack directory, the
//!   facts file, running compose.
//! * `pct`: the wrapper as PVE sees it: config, status, exec, push.
//! * `doc`: the `compose` subtree of the guest's pve-meta document.
//! * `config`: `/etc/pve/pve-compose.cfg`, the operator's persisted choices.
//! * `prompt`: questions for a human, asked before anything is done.
//! * `cmd`, `size`, `lock`: helpers.
//!
//! Facts about what was applied live on the stack disk inside the guest
//! (`stack::Facts`), never in the document: the document is intent.

pub mod cmd;
pub mod config;
pub mod doc;
pub mod lock;
pub mod ops;
pub mod pct;
pub mod plan;
pub mod prompt;
pub mod size;
pub mod spec;
pub mod stack;

/// Where the stack lives inside the guest, on the rootfs. Fixed: it is the
/// one path the operator, the rendered compose file and a human at the shell
/// all agree on.
pub const STACK_DIR: &str = "/opt/stack";
/// Managed volumes are directories under here, one per compose volume name.
pub const VOLUMES_DIR: &str = "/opt/stack/volumes";
/// The operator's own files inside the stack directory.
pub const FACTS_DIR: &str = "/opt/stack/.pve-compose";
/// The pve-meta prefix this operator reads. Fixed: the prefix file the
/// package ships declares it, and its selector says which guests it reaches.
pub const PREFIX: &str = "compose";
/// The tag the packaged prefix file selects on; the fallback when the
/// registry cannot be read. The live value comes from `GET /meta/prefixes`.
pub const DEFAULT_TAG: &str = "compose";
