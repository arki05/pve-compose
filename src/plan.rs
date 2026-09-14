//! The pct-level plan: what turns the wrapper's config into what the document
//! asks for. Pure: reads a `GuestConfig` and a `Doc`, produces operations
//! and notes. `execute` runs the operations through `pct`.
//!
//! Rules (docs/DESIGN.md):
//! * a managed disk is identified by its mount path, never by its index;
//! * disks grow, never shrink, never move, are never deleted by this tool;
//! * a mount under the volumes directory with no volume in the document is
//!   an orphan: reported, kept.

use std::fmt;

use anyhow::{bail, Result};

use crate::config::Config;
use crate::doc::Doc;
use crate::pct::{self, GuestConfig, Mount};
use crate::size::Size;
use crate::spec::{Kind, Volume};
use crate::{STACK_DIR, VOLUMES_DIR};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    SetFeatures(String),
    AddTag(String),
    CreateDisk {
        mp: String,
        storage: String,
        size: Size,
        backup: bool,
    },
    CreateBind {
        mp: String,
        path: String,
    },
    Resize {
        key: String,
        mp: String,
        from: Size,
        to: Size,
    },
    /// Rewrites an existing mount line with a changed `backup` flag.
    SetBackup {
        mount: Mount,
        backup: bool,
    },
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::SetFeatures(s) => write!(f, "features: {s} (takes effect on restart)"),
            Op::AddTag(t) => write!(f, "tag: add '{t}'"),
            Op::CreateDisk {
                mp,
                storage,
                size,
                backup,
            } => {
                write!(
                    f,
                    "disk: create {size} on {storage} at {mp}{}",
                    if *backup { "" } else { " (backup=0)" }
                )
            }
            Op::CreateBind { mp, path } => write!(f, "bind: {path} at {mp}"),
            Op::Resize { key, mp, from, to } => write!(f, "disk: grow {key} ({mp}) {from} -> {to}"),
            Op::SetBackup { mount, backup } => {
                write!(
                    f,
                    "disk: {} ({}) backup={}",
                    mount.key,
                    mount.mp.as_deref().unwrap_or("?"),
                    u8::from(*backup)
                )
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub ops: Vec<Op>,
    /// Things the plan does not do but the operator should know.
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Every setting the plan derives from the document, config and node.
pub struct Inputs<'a> {
    pub node: &'a str,
    pub config: &'a Config,
    /// The tag the prefix selects on; `None` when it reaches every guest.
    pub tag: Option<&'a str>,
    pub guest: &'a GuestConfig,
    pub doc: &'a Doc,
    pub volumes: &'a [Volume],
}

fn disk_size_satisfied(have: Option<Size>, want: Size) -> bool {
    // PVE reports the size it has; a fraction of a GiB rounds in the tool
    // that created it, so a difference under 1 MiB is no difference.
    match have {
        Some(h) => h.bytes() + (1 << 20) >= want.bytes(),
        None => false,
    }
}

pub fn build(i: &Inputs<'_>) -> Result<Plan> {
    let mut plan = Plan::default();
    let g = i.guest;
    let d = i.doc;

    if g.is_template {
        bail!("guest {} is a template", g.vmid);
    }
    if !g.unprivileged {
        plan.notes.push(
            "guest is privileged; pve-compose expects unprivileged wrappers, continuing".into(),
        );
    }

    // The two features docker needs. Everything else about the wrapper
    // (cores, memory, other features, network) is PVE config and not this
    // tool's to touch.
    {
        let mut want = g.features.clone();
        want.entry("nesting".into()).or_insert_with(|| "1".into());
        want.entry("keyctl".into()).or_insert_with(|| "1".into());
        if want != g.features {
            let s = want
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            plan.ops.push(Op::SetFeatures(s));
        }
    }

    if let Some(t) = i.tag {
        if !g.has_tag(t) {
            plan.ops.push(Op::AddTag(t.to_string()));
        }
    }

    let default_storage = i.config.storage_for(i.node);

    // The stack disk.
    let stack_storage = d
        .stack
        .storage
        .clone()
        .unwrap_or_else(|| default_storage.clone());
    let stack_size = d.stack.size.unwrap_or(i.config.defaults.stack_size);
    match g.mount_at(STACK_DIR) {
        None => plan.ops.push(Op::CreateDisk {
            mp: STACK_DIR.into(),
            storage: stack_storage,
            size: stack_size,
            backup: true,
        }),
        Some(m) => {
            if m.is_bind() {
                plan.notes.push(format!(
                    "{STACK_DIR} is a bind mount of {}; leaving it",
                    m.volume
                ));
            } else {
                if let Some(st) = m.storage() {
                    if st != stack_storage {
                        plan.notes.push(format!(
                            "{STACK_DIR} is on {st}, document says {stack_storage}; disks are never moved"
                        ));
                    }
                }
                if !disk_size_satisfied(m.size, stack_size) {
                    plan.ops.push(Op::Resize {
                        key: m.key.clone(),
                        mp: STACK_DIR.into(),
                        from: m.size.unwrap_or(Size(0)),
                        to: stack_size,
                    });
                }
                if !m.backup {
                    plan.ops.push(Op::SetBackup {
                        mount: m.clone(),
                        backup: true,
                    });
                }
            }
        }
    }

    // Managed volumes.
    for v in i.volumes {
        let dir = v.dir();
        match &v.kind {
            Kind::Plain => {
                if let Some(m) = g.mount_at(&dir) {
                    plan.notes.push(format!(
                        "volume {}: document says plain directory but {} is mounted at {dir}; leaving it",
                        v.name, m.key
                    ));
                }
            }
            Kind::Bind { path } => match g.mount_at(&dir) {
                None => plan.ops.push(Op::CreateBind {
                    mp: dir,
                    path: path.clone(),
                }),
                Some(m) => {
                    if !m.is_bind() {
                        plan.notes.push(format!(
                            "volume {}: document says bind of {path} but {} at {dir} is a disk; leaving it",
                            v.name, m.key
                        ));
                    } else if m.volume != *path {
                        plan.notes.push(format!(
                            "volume {}: bound to {} but document says {path}; change it by hand",
                            v.name, m.volume
                        ));
                    }
                }
            },
            Kind::Disk { storage, size } => {
                let storage = storage.clone().unwrap_or_else(|| default_storage.clone());
                match g.mount_at(&dir) {
                    None => plan.ops.push(Op::CreateDisk {
                        mp: dir,
                        storage,
                        size: *size,
                        backup: v.backup,
                    }),
                    Some(m) => {
                        if m.is_bind() {
                            plan.notes.push(format!(
                                "volume {}: document says a disk but {} at {dir} is a bind of {}; leaving it",
                                v.name, m.key, m.volume
                            ));
                            continue;
                        }
                        if let Some(st) = m.storage() {
                            if st != storage {
                                plan.notes.push(format!(
                                    "volume {}: on {st}, document says {storage}; disks are never moved",
                                    v.name
                                ));
                            }
                        }
                        if !disk_size_satisfied(m.size, *size) {
                            plan.ops.push(Op::Resize {
                                key: m.key.clone(),
                                mp: dir,
                                from: m.size.unwrap_or(Size(0)),
                                to: *size,
                            });
                        } else if let Some(h) = m.size {
                            if h.bytes() > size.bytes() + (1 << 20) {
                                plan.notes.push(format!(
                                    "volume {}: is {h}, document says {size}; disks never shrink",
                                    v.name
                                ));
                            }
                        }
                        if m.backup != v.backup {
                            plan.ops.push(Op::SetBackup {
                                mount: m.clone(),
                                backup: v.backup,
                            });
                        }
                    }
                }
            }
        }
    }

    // Orphans: mounted under the volumes directory, not in the document.
    let prefix = format!("{VOLUMES_DIR}/");
    for m in &g.mounts {
        if let Some(mp) = &m.mp {
            if let Some(name) = mp.strip_prefix(&prefix) {
                if !i.volumes.iter().any(|v| v.name == name) {
                    plan.notes.push(format!(
                        "orphan: {} at {mp} has no volume '{name}' in the document; kept (remove it by hand)",
                        m.key
                    ));
                }
            }
        }
    }

    Ok(plan)
}

/// Runs the plan against the guest. A create allocates the next free `mpN`
/// at execution time, since earlier creates in the same plan take indexes.
pub fn execute(guest: &GuestConfig, plan: &Plan) -> Result<()> {
    let mut next = guest.free_mp_index();
    let used: Vec<u32> = guest.mounts.iter().filter_map(Mount::index).collect();
    let mut take_index = || -> u32 {
        while used.contains(&next) {
            next += 1;
        }
        let i = next;
        next += 1;
        i
    };
    for op in &plan.ops {
        eprintln!("  {op}");
        match op {
            Op::SetFeatures(s) => pct::set(guest.vmid, &["--features", s])?,
            Op::AddTag(t) => {
                let mut tags = guest.tags.clone();
                tags.push(t.clone());
                pct::set(guest.vmid, &["--tags", &tags.join(";")])?
            }
            Op::CreateDisk {
                mp,
                storage,
                size,
                backup,
            } => {
                let idx = take_index();
                let spec = format!(
                    "{storage}:{},mp={mp},backup={}",
                    size.to_pct_gib(),
                    u8::from(*backup)
                );
                pct::set(guest.vmid, &[&format!("--mp{idx}"), &spec])?
            }
            Op::CreateBind { mp, path } => {
                let idx = take_index();
                let spec = format!("{path},mp={mp}");
                pct::set(guest.vmid, &[&format!("--mp{idx}"), &spec])?
            }
            Op::Resize { key, to, .. } => pct::resize(guest.vmid, key, *to)?,
            Op::SetBackup { mount, backup } => {
                let mut m = mount.clone();
                m.backup = *backup;
                pct::set(guest.vmid, &[&format!("--{}", m.key), &m.to_string()])?
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec;

    fn guest(json: &str) -> GuestConfig {
        pct::parse_config(9, &serde_json::from_str(json).unwrap()).unwrap()
    }

    fn doc(yaml: &str) -> Doc {
        crate::doc::parse(yaml).unwrap()
    }

    #[test]
    fn size_tolerance_is_one_mib() {
        let want = Size(20 << 30);
        assert!(disk_size_satisfied(Some(want), want));
        assert!(disk_size_satisfied(
            Some(Size(want.bytes() - (1 << 20))),
            want
        ));
        assert!(!disk_size_satisfied(
            Some(Size(want.bytes() - (1 << 20) - 1)),
            want
        ));
        assert!(disk_size_satisfied(Some(Size(want.bytes() + 1)), want));
        assert!(!disk_size_satisfied(None, want));
    }

    #[test]
    fn fresh_wrapper_gets_everything() {
        let g = guest(
            r#"{"unprivileged":1,"rootfs":"local-lvm:vm-9-disk-0,size=6G","features":"nesting=1"}"#,
        );
        let d = doc("spec:\n  volumes:\n    db: { x-pve: { size: 20G } }\n    media: { x-pve: { path: /m } }\n    c:\n");
        let vols = spec::volumes(&d.spec).unwrap();
        let cfg = Config::default();
        let p = build(&Inputs {
            node: "n",
            config: &cfg,
            tag: Some("compose"),
            guest: &g,
            doc: &d,
            volumes: &vols,
        })
        .unwrap();
        let s: Vec<String> = p.ops.iter().map(|o| o.to_string()).collect();
        assert!(
            s.iter()
                .any(|x| x.starts_with("features: keyctl=1,nesting=1")),
            "{s:?}"
        );
        assert!(s.contains(&"tag: add 'compose'".to_string()));
        assert!(s.contains(&"disk: create 4G on local-lvm at /opt/stack".to_string()));
        assert!(s.contains(&"disk: create 20G on local-lvm at /opt/stack/volumes/db".to_string()));
        assert!(s.contains(&"bind: /m at /opt/stack/volumes/media".to_string()));
        assert_eq!(p.ops.len(), 5);
    }

    #[test]
    fn converged_wrapper_is_empty() {
        let g = guest(
            r#"{"unprivileged":1,"tags":"compose","features":"nesting=1,keyctl=1",
            "mp0":"local-lvm:vm-9-disk-1,mp=/opt/stack,size=4G,backup=1",
            "mp1":"local-lvm:vm-9-disk-2,mp=/opt/stack/volumes/db,size=20G,backup=1"}"#,
        );
        let d = doc("spec:\n  volumes:\n    db: { x-pve: { size: 20G } }\n");
        let vols = spec::volumes(&d.spec).unwrap();
        let cfg = Config::default();
        let p = build(&Inputs {
            node: "n",
            config: &cfg,
            tag: Some("compose"),
            guest: &g,
            doc: &d,
            volumes: &vols,
        })
        .unwrap();
        assert!(p.is_empty(), "{:?}", p.ops);
        assert!(p.notes.is_empty(), "{:?}", p.notes);
    }

    #[test]
    fn grows_never_shrinks_and_reports_orphans() {
        let g = guest(
            r#"{"unprivileged":1,"tags":"compose","features":"nesting=1,keyctl=1",
            "mp0":"local-lvm:vm-9-disk-1,mp=/opt/stack,size=4G,backup=1",
            "mp1":"local-lvm:vm-9-disk-2,mp=/opt/stack/volumes/db,size=10G,backup=1",
            "mp2":"local-lvm:vm-9-disk-3,mp=/opt/stack/volumes/old,size=1G,backup=1",
            "mp3":"local-lvm:vm-9-disk-4,mp=/opt/stack/volumes/big,size=50G,backup=1"}"#,
        );
        let d = doc("spec:\n  volumes:\n    db: { x-pve: { size: 20G } }\n    big: { x-pve: { size: 10G, backup: 0 } }\n");
        let vols = spec::volumes(&d.spec).unwrap();
        let cfg = Config::default();
        let p = build(&Inputs {
            node: "n",
            config: &cfg,
            tag: Some("compose"),
            guest: &g,
            doc: &d,
            volumes: &vols,
        })
        .unwrap();
        let s: Vec<String> = p.ops.iter().map(|o| o.to_string()).collect();
        assert_eq!(
            s,
            vec![
                "disk: grow mp1 (/opt/stack/volumes/db) 10G -> 20G",
                "disk: mp3 (/opt/stack/volumes/big) backup=0",
            ]
        );
        assert!(p.notes.iter().any(|n| n.starts_with("orphan: mp2")));
        assert!(p.notes.iter().any(|n| n.contains("never shrink")));
    }

    #[test]
    fn node_storage_default_applies() {
        let g = guest(r#"{"unprivileged":1,"tags":"compose","features":"nesting=1,keyctl=1"}"#);
        let d = doc("spec: {}\n");
        let cfg: Config = serde_yaml_ng::from_str("nodes:\n  n: { storage: NetApp }\n").unwrap();
        let p = build(&Inputs {
            node: "n",
            config: &cfg,
            tag: Some("compose"),
            guest: &g,
            doc: &d,
            volumes: &[],
        })
        .unwrap();
        assert_eq!(
            p.ops[0].to_string(),
            "disk: create 4G on NetApp at /opt/stack"
        );
    }
}
