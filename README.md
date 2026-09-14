# pve-compose

One docker compose stack per LXC, described by the `compose` subtree of the
guest's [pve-meta](https://github.com/arki05/pve-meta) document, reconciled by a
loop on the owning node. The container is a wrapper: its rootfs holds Debian and
docker, the stack and its volumes are PVE disks of their own.

Reference for using it. Why it is shaped this way: [`docs/DESIGN.md`](docs/DESIGN.md).
What is deliberately not in it yet: [`docs/ROADMAP.md`](docs/ROADMAP.md).

## How it works

A guest tagged `compose` with a document like this:

```yaml
# pve-meta document of CT 105, subtree `compose`
stack:  { storage: NetApp, size: 8G }
policy: { pct: auto, docker: auto, pull: manual }
spec:
  services:
    app:
      image: ghcr.io/example/app
      user: "${PVE_UID}:${PVE_GID}"
      ports: ["8080:8080"]
      volumes: ["db:/var/lib/postgresql", "media:/data", "cache:/cache"]
  volumes:
    db:    { x-pve: { storage: NetApp, size: 20G } }
    media: { x-pve: { path: /NetApp/FileStore/Media } }
    cache: {}
```

gets this inside:

```
/opt/stack/                mp0: the stack disk (8G on NetApp)
  compose.yaml             rendered from spec; overwritten on every apply
  .env                     yours; the tool never reads or writes it
  volumes/
    db/                    mp1: a 20G disk on NetApp, backed up with the guest
    media/                 mp2: bind of the host path
    cache/                 a directory on the stack disk
  .pve-compose/applied.yaml   facts: digest applied, template, when
```

and `cd /opt/stack && docker compose up -d` is what runs, by the tool or by you.

Two halves, in the CLI and in `policy`:

| | `pct` | `docker` |
|---|---|---|
| acts through | `pct` on the node | `pct exec` inside the guest |
| `apply` | stack disk, managed volumes, features `nesting`+`keyctl`, the tag | docker if missing, render and push `compose.yaml`, `compose up -d` |
| `diff` | the plan | line diff of `compose.yaml` |
| `status` | pending pct changes | applied / pending / down, containers |
| `upgrade` | — | `compose pull`, then apply |
| `template` | print/fetch the template used | — |
| other verbs | — | passed to `docker compose` in `/opt/stack` |

A bare verb runs both halves, pct first. A vmid on another node is forwarded
over PVE's inter-node SSH and runs there.

## Install

On every node, after pve-meta:

```sh
apt install ./pve-compose_<version>_amd64.deb
install -m 0644 /usr/share/doc/pve-compose/pve-compose.cfg.example /etc/pve/pve-compose.cfg
$EDITOR /etc/pve/pve-compose.cfg        # cluster-wide; set the default storage per node
```

Installs `/usr/sbin/pve-compose`, the `pve-compose.service` loop (enabled and
started), and the prefix file `/usr/share/pve-meta/prefixes/compose.yaml`.

## Commands

```sh
pve-compose new <vmid> --name <host> [--storage S] [--ip dhcp|CIDR] [--gateway G]
                       [--bridge B] [--cores N] [--memory MiB] [--rootfs-size 8G]
                       [--stack-size 4G] [--template T] [--no-up]
pve-compose status [<vmid>] [--json]
pve-compose diff <vmid>
pve-compose apply <vmid> [--reboot] [--no-up]
pve-compose upgrade <vmid> [--reboot]
pve-compose pct apply|diff|status <vmid>
pve-compose pct template
pve-compose docker apply|diff|status|upgrade <vmid>
pve-compose docker <compose-verb> <vmid> [args...]     # logs, ps, exec, pull, down, ...
pve-compose daemon                                     # what the unit runs
-v on any verb echoes every command run
```

* `new` creates an unprivileged wrapper from the template, tags it, writes a
  document with the default policy and an empty `spec`, starts it, installs
  docker (with a one-off `hello-world` run as the smoke test), applies. Then
  write the stack into `compose.spec`, put data under `/opt/stack/volumes/`
  and secrets in `/opt/stack/.env`. With no services in the document nothing
  is started; `--no-up` keeps it that way after you add some, until a
  `pve-compose docker apply <vmid>`. Only `--storage` and `--stack-size` are
  written into the document; everything else is PVE config.
* `apply` on a guest whose features had to be set stops after the pct half:
  features take effect on restart. `--reboot` restarts it and continues.
  `--no-up` renders and pushes the file without starting the stack.
* Stopped guests are refused by every verb but `new`.

## The document

`stack`

| key | meaning | default |
|---|---|---|
| `storage` | storage of the stack disk, and of `x-pve` disks that name none | config, per node |
| `size` | stack disk size, grow-only | config `stack_size` |
| `owner` | `uid:gid` of volume directories; `${PVE_UID}`/`${PVE_GID}` | config `owner` |
| `project` | compose project name; `docker down` before changing it on a running stack, or the old project keeps running beside the new | hostname, lowercased, invalid characters to `-` |

`policy` (what the loop does on its own; a hand-run verb always does everything)

| key | values | default | meaning |
|---|---|---|---|
| `pct` | `auto` `manual` | `auto` | reconcile disks, features, tag on document change |
| `docker` | `auto` `manual` | `auto` | render and push the file; `compose up` only if the stack is already up |
| `pull` | `auto` `manual` | `manual` | `compose pull` before the loop's `up` |

`spec`: the compose document, with:

* `${PVE_VMID}`, `${PVE_NAME}` (hostname), `${PVE_UID}`, `${PVE_GID}` and
  `${PVE_STACK}` (`/opt/stack`) substituted at render time, so the rendered
  file is a function of the document and the config alone. Every other
  `${VAR}` is compose's, from `/opt/stack/.env`; an app that needs the guest's
  own address gets it there.
* a top-level `name:` set to the project, so a plain `docker compose` in
  `/opt/stack` is the same stack.
* named volumes: every one becomes `/opt/stack/volumes/<name>` and is bound
  there in the rendered file. `x-pve` says what backs it:

| `x-pve` | backing |
|---|---|
| `{ storage?, size, owner?, backup? }` | a PVE disk mounted there; `backup` default on |
| `{ path, owner? }` | a bind of that host path (`backup` refused: PVE never backs up binds) |
| none (`name: {}`) | a directory on the stack disk |
| `external: true` on the volume | left alone |

Disks are identified by mount path, not index. They grow when `size` grows,
never shrink, never move to another storage, and are never deleted: a volume
gone from the document leaves an orphan mount that `diff` and `apply` report
until you `pct set --delete` it. A bind-backed volume does not get an image's
initial content the way a native docker volume does.

pve-meta limits that show up here: a bare `cache:` is a null and is refused,
write `cache: {}`; dotted map keys (`driver_opts` and the map forms of `labels`
and `sysctls`) are refused, use the list forms where they exist.

Not in the document: cores, memory, network, `onboot`, extra features. Those
are the guest's PVE config.

## `/etc/pve/pve-compose.cfg`

Cluster-wide, every key optional; the example file lists them all.

| key | default | meaning |
|---|---|---|
| `template.storage` | `local` | a storage with `vztmpl` content |
| `template.base` | `debian-13-standard` | upstream template name prefix; fetched with `pveam` if missing |
| `defaults.storage` | `local-lvm` | stack disk and managed volumes |
| `defaults.stack_size` | `4G` | |
| `defaults.rootfs_size` | `8G` | |
| `defaults.owner` | `1000:1000` | |
| `defaults.bridge` | `vmbr0` | |
| `defaults.cores` / `memory` / `swap` | `2` / `2048` / `0` | for `new` |
| `nodes.<name>.storage` / `.bridge` | | per-node override of the two |
| `daemon.interval` | `30` | seconds between version-token polls |
| `daemon.full_every` | `600` | seconds between passes over every guest |

The tag the tool watches is the `selector` of the `compose` prefix file
(packaged, or the override at `/etc/pve/meta.d/prefixes/compose.yaml`);
`{ all: true }` means every guest with a document.

## The loop

One per node; only this node's running, selected guests. Polls
`GET /meta/version`; on a change, or every `full_every` seconds regardless, plans
every such guest and applies what `policy` allows. The docker half applies a
changed document to a stack that is up; on a stack that is down it writes the
file and starts nothing. Starting is always a hand-run `docker apply`.

It never: starts a stack, pulls with `pull: manual`, reboots a guest,
acts on a stopped guest, deletes or shrinks a disk, touches another node's
guest. One guest at a time; a failed guest is retried on the next document
change or full pass. A per-guest lock in `/run/lock/pve-compose/` serialises the
loop and hand-run verbs.

## Migrating a stack by hand

There is no migrate verb (see the roadmap). To move a stack onto a fresh
wrapper, or to another storage:

```sh
pve-compose new <new> --name <host> --no-up [--storage S]   # wrapper, stack disk, docker; nothing started
pve-meta get <old> --format yaml > doc.yaml                  # the whole document, every prefix
pve-meta set <new> --file doc.yaml
pve-compose pct apply <new>                                  # the real document's volume disks, hotplugged
pve-compose docker down <old>; pct stop <old>
pct stop <new>
pct mount <old>; pct mount <new>
cp -a --reflink=auto /var/lib/lxc/<old>/rootfs/opt/stack/. /var/lib/lxc/<new>/rootfs/opt/stack/
pct unmount <new>; pct unmount <old>
pct start <new>
pve-compose docker apply <new>
pct set <old> --onboot 0 --delete tags                       # keep it stopped and out of the loop
```

`pct mount` mounts the data volumes under the rootfs, so one `cp` of
`/opt/stack` covers the stack disk and every managed volume, provided
`pct apply` created them on the new guest first; `--reflink=auto` is free on
btrfs and on ZFS pools with block cloning. Both guests stopped for the copy.
Do not run both guests against the same host binds.

## Templates

The newest upstream `<template.base>_*_<arch>.tar.zst` on `template.storage`,
downloaded with `pveam` when absent. Docker is installed by the tool
(get.docker.com, about two minutes) on `new` and on `docker apply` when missing.
`pct template` prints what would be used.

## Files

| where | what |
|---|---|
| `/usr/sbin/pve-compose` | the binary |
| `/usr/lib/systemd/system/pve-compose.service` | the loop |
| `/usr/share/pve-meta/prefixes/compose.yaml` | the prefix declaration and schema |
| `/etc/pve/pve-compose.cfg` | operator config, cluster-wide |
| `/run/lock/pve-compose/<vmid>.lock` | per-guest lock, per node |
| in the guest: `/opt/stack/` | see above |

Facts about what was applied live in the guest, never in the document; the
document is intent only.

## Building and releasing

```sh
make check      # fmt, clippy -D warnings, tests
make deb        # dpkg-buildpackage on a Debian 13 host with a Rust toolchain
```

Version: `Cargo.toml` and the upstream part of `debian/changelog`'s top entry
must match (the package build checks). `debian/changelog` is the changelog;
there is no other.

Releasing, the same way as pve-meta and pve-meta-traefik:

1. Bump `version` in `Cargo.toml` (and `Cargo.lock`), add a `debian/changelog`
   entry with the same upstream version (`dch -v 0.2.0-1`), commit.
2. `git tag v0.2.0 && git push origin main v0.2.0`.
3. `.github/workflows/build.yml` builds and tests on amd64 and arm64 in a
   `debian:trixie` container on every push; on a `v*` tag it checks the tag
   against the changelog, publishes the `.deb`s as a GitHub Release (a file
   name already published is never uploaded again), and asks
   `apt.arki05.com` to index them if the `APT_REPO_DISPATCH_TOKEN` secret is
   set, otherwise its nightly run picks them up. That repository lists
   `arki05/pve-compose` in its `packages.txt`.
4. On the nodes: `apt update && apt install pve-compose`.

`.github/workflows/audit.yml` runs `cargo audit` against the lock file weekly
and on dependency changes.

AGPL-3.0-or-later.
