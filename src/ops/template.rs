//! Which template `new` unpacks: the newest upstream Debian
//! template on the template storage, downloaded through `pveam` if there is
//! none yet. Docker is installed afterwards by `provision`, every time, so
//! there is no docker-ready template to build or keep fresh.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::cmd;
use crate::ops::Ctx;

/// The node's Debian architecture name, as template file names carry it.
pub fn arch() -> Result<String> {
    let out = cmd::run("dpkg", &["--print-architecture"])?;
    let a = out.stdout.trim().to_string();
    if a.is_empty() {
        bail!("cannot determine the architecture");
    }
    Ok(a)
}

/// Whether a template file name is for this node's architecture.
fn for_arch(name: &str, arch: &str) -> bool {
    name.contains(&format!("_{arch}."))
}

/// The newest vztmpl on the template storage whose file name starts with
/// `prefix`, for this node's architecture.
pub fn newest_vztmpl(ctx: &Ctx, prefix: &str) -> Result<Option<String>> {
    let st = &ctx.config.template.storage;
    let arch = arch()?;
    let v = cmd::pvesh_get(
        &format!("/nodes/{}/storage/{st}/content", ctx.node),
        &["--content", "vztmpl"],
    )?;
    let mut names: Vec<String> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("volid").and_then(Value::as_str))
                .filter(|id| {
                    id.rsplit('/')
                        .next()
                        .map(|n| n.starts_with(prefix) && for_arch(n, &arch))
                        .unwrap_or(false)
                })
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    Ok(names.pop())
}

/// Downloads the newest upstream template matching the configured base.
pub fn download_base(ctx: &Ctx) -> Result<String> {
    let base = &ctx.config.template.base;
    let st = &ctx.config.template.storage;
    let arch = arch()?;
    eprintln!("template: fetching the {base} template list");
    cmd::run("pveam", &["update"])?;
    let out = cmd::run("pveam", &["available", "--section", "system"])?;
    let mut names: Vec<&str> = out
        .stdout
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|n| n.starts_with(&format!("{base}_")) && for_arch(n, &arch))
        .collect();
    names.sort();
    let Some(name) = names.pop() else {
        bail!("no upstream {arch} template starts with {base}_ (pveam available)");
    };
    eprintln!("template: downloading {name} to {st}");
    cmd::stream("pveam", &["download", st, name])?;
    Ok(format!("{st}:vztmpl/{name}"))
}

/// Resolves the vztmpl volid to use. `explicit` may be a volid or a
/// file-name prefix on the template storage. With nothing given: the newest
/// configured base template, downloaded if missing.
pub fn resolve(ctx: &Ctx, explicit: Option<&str>) -> Result<String> {
    if let Some(e) = explicit {
        if e.contains(":vztmpl/") {
            return Ok(e.into());
        }
        if let Some(v) = newest_vztmpl(ctx, e)? {
            return Ok(v);
        }
        bail!(
            "no template on {} starts with {e}",
            ctx.config.template.storage
        );
    }
    if let Some(v) = newest_vztmpl(ctx, &format!("{}_", ctx.config.template.base))? {
        return Ok(v);
    }
    download_base(ctx)
}

#[cfg(test)]
mod tests {
    use super::for_arch;

    #[test]
    fn architecture_filter() {
        assert!(for_arch("debian-13-standard_13.6-1_amd64.tar.zst", "amd64"));
        assert!(!for_arch(
            "debian-13-standard_13.6-1_arm64.tar.zst",
            "amd64"
        ));
    }
}
