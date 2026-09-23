# pve-compose

One docker compose stack per LXC, described by the `compose` subtree of the
guest's [pve-meta](https://github.com/arki05/pve-meta) document, reconciled by a
loop on the owning node. The container is a wrapper: its rootfs holds Debian,
docker and the stack directory; volumes that matter are PVE disks of their own.

Reference for using it. Why it is shaped this way: [`docs/DESIGN.md`](docs/DESIGN.md).
What is deliberately not in it yet: [`docs/ROADMAP.md`](docs/ROADMAP.md).

## How it works

A guest tagged `compose` with a document like this:

```yaml
# pve-meta document of CT 105, subtree `compose`
policy: { pct: auto, docker: auto, up: manual, pull: manual }
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
/opt/stack/                on the rootfs
  compose.yaml             rendered from spec; overwritten on every apply
  .env                     yours; the tool never reads or writes it
  volumes/
    db/                    mp1: a 20G disk on NetApp, backed up with the guest
    media/                 mp2: bind of the host path
    cache/                 a directory on the rootfs
  .pve-compose/applied.yaml   facts: digest applied, template, when
```

and `cd /opt/stack && docker compose up -d` is what runs, by the tool or by you.

Two halves, in the CLI and in `policy`:

| | `pct` | `docker` |
|---|---|---|
| acts through | `pct` on the node | `pct exec` inside the guest |
| `apply` | managed volumes, features `nesting`+`keyctl`, the tag | docker if missing, render and push `compose.yaml`, `compose up -d` |
| `diff` | the plan | line diff of `compose.yaml` |
| `status` | pending pct changes | applied / pending / down, containers |
| `upgrade` | — | `compose pull`, then apply |
| other verbs | — | passed to `docker compose` in `/opt/stack` |

A bare verb runs both halves, pct first. A vmid on another node is forwarded
over PVE's inter-node SSH and runs there.

## Install

On every node, after pve-meta:

```sh
apt update && apt install pve-compose      # from apt.arki05.com
```

Installs `/usr/sbin/pve-compose`, the `pve-compose.service` loop (enabled and
started), and the prefix file `/usr/share/pve-meta/prefixes/compose.yaml`. No
configuration is needed up front: the first `new` on a node asks which storage
to use for templates and for disks and offers to remember the answers.

## Commands

```sh
pve-compose new <vmid> --name <host> [--storage S] [--ip dhcp|CIDR] [--gateway G]
                       [--bridge B] [--cores N] [--memory MiB] [--rootfs-size 8G]
                       [--template T] [--no-up]
pve-compose status [<vmid>] [--json]
pve-compose diff <vmid>
pve-compose apply <vmid> [--no-up]
pve-compose upgrade <vmid>
pve-compose pct apply|diff|status <vmid>
pve-compose docker apply|diff|status|upgrade <vmid>
pve-compose docker <compose-verb> <vmid> [args...]     # logs, ps, exec, pull, down, ...
pve-compose config                                     # the stored choices and where each comes from
pve-compose config set|unset <key> [value]             # storage (this node), template-storage, template-base,
                                                       # owner, bridge, cores, memory, rootfs-size
pve-compose daemon                                     # what the unit runs
-v on any verb echoes every command run
```

* `new` settles everything before it touches anything: the template storage
  and the disk storage come from a flag, from the stored choice, or from a
  numbered question at the terminal, and an answer is offered for storing.
  Without a terminal it stops and names the `config set` command instead.
  Then it creates an unprivileged wrapper from the template, tags it, writes a
  document with the default policy and an empty `spec` (refusing if that vmid
  already has a document, rather than replacing it), starts it, installs
  docker (with a one-off `hello-world` run as the smoke test), applies. Write
  the stack into `compose.spec`, put data under `/opt/stack/volumes/` and
  secrets in `/opt/stack/.env`. With no services in the document nothing is
  started; `--no-up` keeps it that way after you add some, until a
  `pve-compose docker apply <vmid>`. Nothing from the flags goes into the
  document; the wrapper's settings are PVE config.
* `apply` on a guest whose features had to be set stops after the pct half:
  features take effect on restart; `pct reboot` it and apply again. `--no-up`
  renders and pushes the file without starting the stack.
* Stopped guests are refused by every verb but `new`.

## The document

`stack`

| key | meaning | default |
|---|---|---|
| `owner` | `uid:gid` of volume directories; `${PVE_UID}`/`${PVE_GID}` | config `owner` |

`policy` (what the loop does on its own; a hand-run verb always does everything)

| key | values | default | meaning |
|---|---|---|---|
| `pct` | `auto` `manual` | `auto` | reconcile disks, features, tag on document change |
| `docker` | `auto` `manual` | `auto` | render and push the file on document change |
| `up` | `auto` `manual` | `manual` | `compose up` on document change, only if the stack is already up; a hand-run `apply` always ups unless `--no-up` |
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

| `x-pve` | backing | bounded by |
|---|---|---|
| `{ storage?, size, owner?, backup? }` | a PVE disk mounted there; `backup` default on; `storage` defaults to this node's stored one, and without either the plan refuses | `limits.storages`, `limits.max_disk_gib` |
| `{ path, owner? }` | a bind of that host path (`backup` refused: PVE never backs up binds) | `limits.bind_roots`; refused outright by default |
| none (`name: {}`) | a directory on the rootfs | — |
| `external: true` on the volume | left alone | — |

The `limits` keys are the node's, not the document's; see "Trust" below.
A volume outside them is refused when the document is read, by name and with
the key that would allow it, and nothing is applied.

Disks are identified by mount path, not index. They grow when `size` grows,
never shrink, never move to another storage, and are never deleted: a volume
gone from the document leaves an orphan mount that `diff` and `apply` report
until you `pct set --delete` it. A bind-backed volume does not get an image's
initial content the way a native docker volume does.

pve-meta comment keys (`image__: why this tag`) never reach the rendered
file: compose reads through `pve-meta get` without `--comments`, which
strips the editor's notes server-side.

pve-meta limits that show up here: a bare `cache:` is a null and is refused,
write `cache: {}`; dotted map keys (`driver_opts` and the map forms of `labels`
and `sysctls`) are refused, use the list forms where they exist.

Not in the document: cores, memory, network, `onboot`, extra features, or
which storage the rootfs is on. Those are the guest's PVE config. The compose
project name is the hostname, lowercased; `docker down` before renaming a
running guest, or the old project keeps running beside the new.

## Trust

Writing a guest's document takes `VM.Config.Options` on that guest — a
routine PVEVMAdmin-tier privilege. pve-compose then acts on that document as
root on the node, so without bounds a document writer would gain two things
PVE does not grant at that tier: bind mounts of any host path (PVE itself
allows those to `root@pam` only), and disks of any size on any storage. A
bind of `/`, of another guest's subvolume, of `/var/lib/vz/dump` or of
`/etc/pve` is the whole node.

Three keys in `/etc/pve/pve-compose.cfg` bound it, and a document outside them
is refused before any plan exists:

| key | scope | default | meaning |
|---|---|---|---|
| `limits.bind_roots` | cluster | empty | host directories an `x-pve.path` may be or be under. Empty: **no document gets a bind mount** |
| `limits.storages` | cluster | empty | storages an `x-pve.storage` may name. Empty: only the storage this node's `storage` names, which is where a volume without one lands anyway |
| `limits.max_disk_gib` | cluster | `64` | the largest disk one `x-pve.size` may ask for |

```yaml
limits:
  bind_roots: [/NetApp/FileStore]
  storages: [NetApp, local-lvm]
  max_disk_gib: 200
```

A path is checked as text: it must be absolute, have no `.`, `..` or empty
segment, and then be one of the roots or lie under one. Storage names are per
node, so `limits.storages` is the union over the cluster; each node still only
allocates on its own.

What remains, with the keys set: a document writer chooses what runs in *their*
container (which is theirs anyway), how much space it takes inside the allowed
storages up to the ceiling, and which of the allowed host directories it binds.
A bind root is shared with every compose guest on the cluster: list directories
you would hand to all of them. Everything else about the wrapper — cores,
memory, network, rootfs storage — is PVE config and needs PVE's own privileges.

## Stored choices

`pve-compose config` shows every choice with its source; `config set` and
`config unset` change one. They write `/etc/pve/pve-compose.cfg`, cluster-wide;
the example file under `/usr/share/doc/pve-compose/` lists the keys, and hand
editing works too.

| key | scope | default | meaning |
|---|---|---|---|
| `storage` | this node | asked by `new`; required for an `x-pve` disk without one | disk storage on this node |
| `template-storage` | cluster | asked by `new` | a storage with `vztmpl` content |
| `template-base` | cluster | `debian-13-standard` | upstream template name prefix; fetched with `pveam` if missing |
| `owner` | cluster | `1000:1000` | volume directory owner |
| `bridge` | cluster | `vmbr0` | for `new` |
| `cores` / `memory` | cluster | `2` / `2048` | for `new` |
| `rootfs-size` | cluster | `8G` | for `new` |

Disk storage is per node on purpose: storage names differ per node, and a
cluster-wide default would be a guess. The file also holds the `limits` keys
above and `daemon.interval` and `daemon.full_every` (30 and 600 seconds), by
hand only.

The tag the tool watches is the `selector` of the `compose` prefix file
(packaged, or the override at `/etc/pve/meta.d/prefixes/compose.yaml`);
`{ all: true }` means every guest with a document.

## The loop

One per node; only this node's running, selected guests. Polls
`GET /meta/version`; on a change, or every `full_every` seconds regardless, plans
every such guest and applies what `policy` allows. The docker half applies a
changed document to a stack that is up; on a stack that is down it writes the
file and starts nothing. Starting is always a hand-run `docker apply`.

Every command it runs has a deadline: 120 seconds for a read, a probe or one
of PVE's quick verbs, 30 minutes for the ones that legitimately take minutes
(a `compose up` that pulls, the docker install, a template download, a disk
allocation). Past it the command's process group is killed and that guest
fails with the guest and the call named, so a hung container cannot hold the
node's other guests. A hand-run `pve-compose docker <verb>` has no deadline:
a human started it and knows how long `logs -f` should run.

The unit is `Type=notify` with `WatchdogSec=45min`, and the loop pings systemd
around every poll and after every guest, so a loop that stops making progress
altogether is restarted. The value is above the longest thing one guest may
legitimately take.

It never: starts a stack, pulls with `pull: manual`, reboots a guest,
acts on a stopped guest, deletes or shrinks a disk, touches another node's
guest. One guest at a time; a failed guest is retried on the next document
change or full pass. A per-guest lock in `/run/lock/pve-compose/` serialises the
loop and hand-run verbs, the passthrough ones included: a `docker logs -f` left
running holds that guest's lock, and the loop says the guest is busy and retries
it on the next pass.

## Migrating a stack by hand

There is no migrate verb (see the roadmap). To move a stack onto a fresh
wrapper, or to another storage:

```sh
pve-compose new <new> --name <host> --no-up [--storage S]   # wrapper, docker; nothing started
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
`/opt/stack` covers the directory and every managed volume, provided
`pct apply` created them on the new guest first; `--reflink=auto` is free on
btrfs and on ZFS pools with block cloning. Both guests stopped for the copy.
Do not run both guests against the same host binds.

## Templates

The newest upstream `<template-base>_*_<arch>.tar.zst` on the template
storage, downloaded with `pveam` when absent. Docker is installed by the tool
(get.docker.com, about two minutes) on `new` and on `docker apply` when missing.

## Files

| where | what |
|---|---|
| `/usr/sbin/pve-compose` | the binary |
| `/usr/lib/systemd/system/pve-compose.service` | the loop |
| `/usr/share/pve-meta/prefixes/compose.yaml` | the prefix declaration and schema |
| `/etc/pve/pve-compose.cfg` | stored choices, cluster-wide, written by `config` and `new` |
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
