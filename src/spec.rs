//! The compose document: which of its volumes this tool manages, and the
//! `compose.yaml` that goes into the guest.
//!
//! Every top-level named volume becomes a directory under
//! `/opt/stack/volumes/<name>`, so a rebuilt wrapper keeps every volume:
//!
//! * `x-pve: { storage, size, owner?, backup? }` -- a PVE disk of its own,
//!   mounted there. Grow-only, never moved, never deleted by this tool.
//! * `x-pve: { path, owner? }` -- a bind mount of a host path.
//!
//! What a document may ask for at that level is not what writing a document
//! takes: [`crate::config::Limits`] bounds the bind paths, the storages and
//! the disk size, and a volume outside them is refused here, before any plan
//! exists (README, "Trust").
//! * no `x-pve` -- a plain directory on the stack disk.
//! * `external: true` -- left alone entirely.
//!
//! Rendering replaces each managed volume's definition with a bind to that
//! directory, drops `x-pve`, and substitutes `${PVE_VMID}`,
//! `${PVE_UID}`, `${PVE_GID}`, `${PVE_STACK}` and `${PVE_NAME}` in every
//! string. Nothing else is touched: the rendered file is the document with
//! those edits, and it runs with a plain `docker compose up -d` in
//! `/opt/stack`, no override file and no extra env file.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_yaml_ng::{Mapping, Value};

use crate::config::{self, Limits};
use crate::doc::flag_opt;
use crate::size::Size;
use crate::VOLUMES_DIR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Disk { storage: Option<String>, size: Size },
    Bind { path: String },
    Plain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub kind: Kind,
    pub owner: Option<String>,
    /// `backup` for a disk: PVE's `backup=1`. Default on.
    pub backup: bool,
}

impl Volume {
    /// The directory inside the guest.
    pub fn dir(&self) -> String {
        format!("{VOLUMES_DIR}/{}", self.name)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct XPve {
    storage: Option<String>,
    size: Option<Size>,
    path: Option<String>,
    owner: Option<String>,
    #[serde(default, deserialize_with = "flag_opt")]
    backup: Option<bool>,
}

/// An absolute path with no empty, `.` or `..` segment: the only shape that
/// can be held against an allow-list root by text alone.
fn check_clean_absolute(path: &str) -> Result<()> {
    let Some(rest) = path.strip_prefix('/') else {
        bail!("'{path}' must be absolute");
    };
    if rest.is_empty() {
        return Ok(());
    }
    for seg in rest.split('/') {
        match seg {
            "" => bail!("'{path}' has an empty segment (no '//', no trailing '/')"),
            "." | ".." => bail!("'{path}' has a '{seg}' segment"),
            _ => {}
        }
    }
    Ok(())
}

/// Whether `path` is `root` or lies under it.
fn under(root: &str, path: &str) -> bool {
    let prefix = if root == "/" {
        "/".to_string()
    } else {
        format!("{root}/")
    };
    path == root || path.starts_with(&prefix)
}

/// A bind path a document asked for, held against `limits.bind_roots`.
fn check_bind_path(name: &str, path: &str, limits: &Limits) -> Result<()> {
    check_clean_absolute(path).with_context(|| format!("spec.volumes.{name}.x-pve.path"))?;
    if limits.bind_roots.is_empty() {
        bail!(
            "spec.volumes.{name}.x-pve.path: host binds are refused; \
             list the directories a document may bind under `limits.bind_roots` in {}",
            config::PATH
        );
    }
    for root in &limits.bind_roots {
        check_clean_absolute(root)
            .with_context(|| format!("{}: limits.bind_roots", config::PATH))?;
        if under(root, path) {
            return Ok(());
        }
    }
    bail!(
        "spec.volumes.{name}.x-pve.path: '{path}' is not under any of `limits.bind_roots` ({}) in {}",
        limits.bind_roots.join(", "),
        config::PATH
    )
}

/// A storage a document named, held against `limits.storages` (already
/// resolved to the node's own storage when the key is unset).
fn check_storage(name: &str, storage: &str, limits: &Limits) -> Result<()> {
    if limits.storages.iter().any(|s| s == storage) {
        return Ok(());
    }
    if limits.storages.is_empty() {
        bail!(
            "spec.volumes.{name}.x-pve.storage: '{storage}' is refused; this node has no disk \
             storage (`pve-compose config set storage <storage>`) and no `limits.storages` in {}",
            config::PATH
        );
    }
    bail!(
        "spec.volumes.{name}.x-pve.storage: '{storage}' is not one of `limits.storages` ({}) in {}",
        limits.storages.join(", "),
        config::PATH
    )
}

/// A disk size a document asked for, held against `limits.max_disk_gib`.
fn check_size(name: &str, size: Size, limits: &Limits) -> Result<()> {
    let ceiling = limits.max_disk_gib << 30;
    if size.bytes() > ceiling {
        bail!(
            "spec.volumes.{name}.x-pve.size: {size} is above `limits.max_disk_gib` ({} GiB) in {}",
            limits.max_disk_gib,
            config::PATH
        );
    }
    Ok(())
}

fn valid_volume_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !name.starts_with('.')
}

/// The managed volumes of a compose document, in declaration order, each
/// held against what the node allows a document to ask for.
pub fn volumes(spec: &Value, limits: &Limits) -> Result<Vec<Volume>> {
    let mut out = Vec::new();
    let Some(vols) = spec.get("volumes") else {
        return Ok(out);
    };
    if vols.is_null() {
        return Ok(out);
    }
    let map = vols
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("spec.volumes is not a map"))?;
    for (k, v) in map {
        let name = k
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("spec.volumes: a volume name is not a string"))?
            .to_string();
        if !valid_volume_name(&name) {
            bail!("spec.volumes.{name}: not a usable directory name");
        }
        let def = match v {
            Value::Null => &Value::Null,
            Value::Mapping(_) => v,
            _ => bail!("spec.volumes.{name}: not a map"),
        };
        if def.get("external").and_then(crate::doc::flag_of) == Some(true) {
            continue;
        }
        let x: Option<XPve> = match def.get("x-pve") {
            None | Some(Value::Null) => None,
            Some(xv) => Some(
                serde_yaml_ng::from_value(xv.clone())
                    .with_context(|| format!("spec.volumes.{name}.x-pve"))?,
            ),
        };
        let (kind, owner, backup) = match x {
            None => (Kind::Plain, None, true),
            Some(x) => {
                if let Some(o) = &x.owner {
                    split_owner(o).with_context(|| format!("spec.volumes.{name}.x-pve.owner"))?;
                }
                let kind = match (&x.path, &x.storage, x.size) {
                    (Some(p), None, None) => {
                        if x.backup.is_some() {
                            bail!("spec.volumes.{name}.x-pve: backup applies to disks; PVE never backs up a bind mount");
                        }
                        check_bind_path(&name, p, limits)?;
                        Kind::Bind { path: p.clone() }
                    }
                    (Some(_), _, _) => {
                        bail!("spec.volumes.{name}.x-pve: path and storage/size are exclusive")
                    }
                    (None, storage, Some(size)) => {
                        check_size(&name, size, limits)?;
                        if let Some(s) = storage {
                            check_storage(&name, s, limits)?;
                        }
                        Kind::Disk {
                            storage: storage.clone(),
                            size,
                        }
                    }
                    (None, Some(_), None) => {
                        bail!("spec.volumes.{name}.x-pve: storage needs a size")
                    }
                    (None, None, None) => Kind::Plain,
                };
                (kind, x.owner, x.backup.unwrap_or(true))
            }
        };
        out.push(Volume {
            name,
            kind,
            owner,
            backup,
        });
    }
    Ok(out)
}

/// The values substituted into the rendered document.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    pub vmid: u32,
    pub uid: String,
    pub gid: String,
    pub name: String,
    /// The compose project name, written as the file's top-level `name:` so
    /// a plain `docker compose` in the stack directory means the same stack.
    pub project: String,
}

impl Vars {
    fn map(&self) -> BTreeMap<&'static str, String> {
        let mut m = BTreeMap::new();
        m.insert("PVE_VMID", self.vmid.to_string());
        m.insert("PVE_UID", self.uid.clone());
        m.insert("PVE_GID", self.gid.clone());
        m.insert("PVE_NAME", self.name.clone());
        m.insert("PVE_STACK", crate::STACK_DIR.to_string());
        m
    }
}

fn substitute(s: &str, vars: &BTreeMap<&'static str, String>) -> String {
    if !s.contains("${PVE_") && !s.contains("$PVE_") {
        return s.to_string();
    }
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("${{{k}}}"), v);
        out = out.replace(&format!("${k}"), v);
    }
    out
}

/// Substitutes `${PVE_*}` vars through the value. Comment keys never reach
/// here: compose reads through `pve-meta get` without `--comments`, which
/// strips the editor's `__` notes server-side, so there is nothing to drop.
fn walk(v: &mut Value, vars: &BTreeMap<&'static str, String>) {
    match v {
        Value::String(s) => *s = substitute(s, vars),
        Value::Sequence(items) => items.iter_mut().for_each(|i| walk(i, vars)),
        Value::Mapping(m) => {
            m.iter_mut().for_each(|(_, val)| walk(val, vars));
        }
        Value::Tagged(t) => walk(&mut t.value, vars),
        _ => {}
    }
}

/// Splits `uid:gid` (or `uid`) into the two numbers as text.
pub fn split_owner(owner: &str) -> Result<(String, String)> {
    let (u, g) = owner.split_once(':').unwrap_or((owner, owner));
    if u.is_empty()
        || g.is_empty()
        || !u.chars().all(|c| c.is_ascii_digit())
        || !g.chars().all(|c| c.is_ascii_digit())
    {
        bail!("owner '{owner}' is not uid:gid");
    }
    Ok((u.to_string(), g.to_string()))
}

/// The `compose.yaml` text for the guest.
pub fn render(spec: &Value, vols: &[Volume], vars: &Vars) -> Result<String> {
    let mut doc = spec.clone();
    walk(&mut doc, &vars.map());
    if let Value::Mapping(top) = &mut doc {
        // First key, so a reader sees the project before anything else.
        let mut with_name = Mapping::new();
        with_name.insert("name".into(), Value::String(vars.project.clone()));
        for (k, v) in top.iter() {
            if k.as_str() != Some("name") {
                with_name.insert(k.clone(), v.clone());
            }
        }
        *top = with_name;
    }
    if let Some(Value::Mapping(map)) = doc.get_mut("volumes") {
        for vol in vols {
            let key = Value::String(vol.name.clone());
            let mut def = Mapping::new();
            def.insert("driver".into(), "local".into());
            let mut opts = Mapping::new();
            opts.insert("type".into(), "none".into());
            opts.insert("o".into(), "bind".into());
            opts.insert("device".into(), Value::String(vol.dir()));
            def.insert("driver_opts".into(), Value::Mapping(opts));
            // Keep a `name:` the author set, and nothing else.
            if let Some(Value::Mapping(old)) = map.get(&key) {
                if let Some(n) = old.get("name") {
                    def.insert("name".into(), n.clone());
                }
            }
            map.insert(key, Value::Mapping(def));
        }
    }
    let text = serde_yaml_ng::to_string(&doc).context("rendering compose.yaml")?;
    Ok(format!(
        "# Rendered by pve-compose from this guest's pve-meta document (compose.spec).\n\
         # Edit the document, not this file: the next apply overwrites it.\n{text}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
services:
  app:
    image: x
    user: "${PVE_UID}:${PVE_GID}"
    volumes: ["db:/var/lib/postgresql", "media:/data", "cache:/cache", "ext:/e"]
volumes:
  db:    { x-pve: { storage: NetApp, size: 20G } }
  media: { x-pve: { path: /NetApp/FileStore/Media, owner: "1500:1500" } }
  cache:
  ext:   { external: true }
"#;

    /// The bounds the SPEC above needs: its bind root and its storage.
    fn limits() -> Limits {
        limits_from("bind_roots: [/NetApp/FileStore]\nstorages: [NetApp]\n")
    }

    fn limits_from(yaml: &str) -> Limits {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    fn volumes_of(yaml: &str, limits: &Limits) -> Result<Vec<Volume>> {
        volumes(&serde_yaml_ng::from_str(yaml).unwrap(), limits)
    }

    #[test]
    fn classifies_volumes() {
        let spec: Value = serde_yaml_ng::from_str(SPEC).unwrap();
        let v = volumes(&spec, &limits()).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "db");
        assert!(
            matches!(&v[0].kind, Kind::Disk { storage: Some(s), size } if s == "NetApp" && size.to_string() == "20G")
        );
        assert!(matches!(&v[1].kind, Kind::Bind { path } if path == "/NetApp/FileStore/Media"));
        assert_eq!(v[1].owner.as_deref(), Some("1500:1500"));
        assert_eq!(v[2].kind, Kind::Plain);
        assert_eq!(v[2].dir(), "/opt/stack/volumes/cache");
    }

    #[test]
    fn volume_names_are_directory_names() {
        assert!(valid_volume_name("db"));
        assert!(valid_volume_name("my_data-1.0"));
        assert!(!valid_volume_name(""));
        assert!(!valid_volume_name("../etc"));
        assert!(!valid_volume_name("a/b"));
        assert!(!valid_volume_name(".hidden"));
        assert!(!valid_volume_name("a b"));
        assert!(!valid_volume_name("it's"));
        assert!(volumes_of("volumes:\n  ../x: {}\n", &limits()).is_err());
    }

    #[test]
    fn refuses_bad_x_pve() {
        let l = limits();
        assert!(volumes_of("volumes:\n  a: { x-pve: { storage: NetApp } }\n", &l).is_err());
        assert!(volumes_of("volumes:\n  a: { x-pve: { path: rel } }\n", &l).is_err());
        assert!(volumes_of("volumes:\n  a: { x-pve: { bogus: 1 } }\n", &l).is_err());
        assert!(volumes_of(
            "volumes:\n  a: { x-pve: { path: /NetApp/FileStore/x, backup: 0 } }\n",
            &l
        )
        .is_err());
        assert!(volumes_of(
            "volumes:\n  a: { x-pve: { size: 1G, owner: \"x;id\" } }\n",
            &l
        )
        .is_err());
    }

    /// A document is written with `VM.Config.Options` on the guest; the host
    /// paths, storages and sizes it may ask for are the node's to allow.
    #[test]
    fn binds_are_refused_outside_the_allowed_roots() {
        let closed = Limits::default();
        let err = volumes_of("volumes:\n  a: { x-pve: { path: /etc } }\n", &closed)
            .unwrap_err()
            .to_string();
        assert!(err.contains("limits.bind_roots"), "{err}");
        assert!(err.contains("refused"), "{err}");

        let l = limits_from("bind_roots: [/srv/media, /tank]\n");
        for good in ["/srv/media", "/srv/media/movies", "/tank/x/y"] {
            let spec = format!("volumes:\n  a: {{ x-pve: {{ path: {good} }} }}\n");
            assert!(volumes_of(&spec, &l).is_ok(), "{good}");
        }
        for bad in [
            "/etc",
            "/srv",
            "/srv/mediaX",
            "/tank/../etc/pve",
            "/srv/media/../../etc",
            "/srv/media/./x",
            "/srv/media/",
            "//srv/media",
            "srv/media",
            "/",
        ] {
            let spec = format!("volumes:\n  a: {{ x-pve: {{ path: \"{bad}\" }} }}\n");
            let err = volumes_of(&spec, &l).unwrap_err().to_string();
            assert!(err.starts_with("spec.volumes.a.x-pve.path"), "{bad}: {err}");
        }

        // A root of `/` is the operator saying so, and takes everything clean.
        let open = limits_from("bind_roots: ['/']\n");
        assert!(volumes_of("volumes:\n  a: { x-pve: { path: /etc } }\n", &open).is_ok());
        assert!(volumes_of("volumes:\n  a: { x-pve: { path: /etc/../x } }\n", &open).is_err());

        // A bind root that is not itself a clean absolute path is the
        // operator's mistake, and is named as one.
        let bad_root = limits_from("bind_roots: [relative]\n");
        let err = volumes_of("volumes:\n  a: { x-pve: { path: /x } }\n", &bad_root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("limits.bind_roots"), "{err}");
    }

    #[test]
    fn storages_and_sizes_are_bounded() {
        let l = limits_from("storages: [NetApp, local-lvm]\nmax_disk_gib: 8\n");
        assert!(volumes_of("volumes:\n  a: { x-pve: { size: 8G } }\n", &l).is_ok());
        assert!(volumes_of(
            "volumes:\n  a: { x-pve: { storage: NetApp, size: 8G } }\n",
            &l
        )
        .is_ok());
        let err = volumes_of(
            "volumes:\n  a: { x-pve: { storage: other, size: 1G } }\n",
            &l,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("limits.storages"), "{err}");
        let err = volumes_of("volumes:\n  a: { x-pve: { size: 9G } }\n", &l)
            .unwrap_err()
            .to_string();
        assert!(err.contains("limits.max_disk_gib"), "{err}");

        // No storage allowed anywhere: named storages are refused with the
        // command that would allow one.
        let err = volumes_of(
            "volumes:\n  a: { x-pve: { storage: NetApp, size: 1G } }\n",
            &Limits::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("config set storage"), "{err}");
    }

    #[test]
    fn renders_binds_and_vars() {
        let spec: Value = serde_yaml_ng::from_str(SPEC).unwrap();
        let v = volumes(&spec, &limits()).unwrap();
        let vars = Vars {
            vmid: 105,
            uid: "1000".into(),
            gid: "1000".into(),
            name: "wiki".into(),
            project: "wiki".into(),
        };
        let text = render(&spec, &v, &vars).unwrap();
        let back: Value = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(back["name"].as_str(), Some("wiki"));
        assert!(
            text.lines().nth(2).unwrap().starts_with("name: wiki"),
            "{text}"
        );
        assert_eq!(back["services"]["app"]["user"].as_str(), Some("1000:1000"));
        assert_eq!(
            back["volumes"]["db"]["driver_opts"]["device"].as_str(),
            Some("/opt/stack/volumes/db")
        );
        assert!(back["volumes"]["db"].get("x-pve").is_none());
        assert_eq!(back["volumes"]["ext"]["external"].as_bool(), Some(true));
        assert!(text.starts_with("# Rendered by pve-compose"));
    }
}
