# pve-compose: design

What is true of the code, and the decisions behind the parts that are not
obvious. The README says how to use it.

## The document is intent; facts live in the guest

`compose.spec` and the few keys beside it describe what should run. What *was*
applied (the document digest, the template the wrapper came from, when) is
written to `/opt/stack/.pve-compose/applied.yaml` inside the guest, not into the
document. Three reasons, all concrete: a status write would move pve-meta's
version token and wake the loop that wrote it; the applied digest cannot be
stored in the thing it digests; and pve-meta's compare-and-swap is per document,
so an operator writing facts while a human edits intent in the UI would make the
human's Apply fail with a 409. Facts under `/opt/stack` move with the guest, roll
back with the guest's snapshot together with the document, and are in the
guest's backup.

If pve-meta grows view-scoped compare-and-swap and a `status` flag on a prefix,
a `compose-status` prefix becomes the nicer home for these facts. Until then the
disk is the honest place.

## No `pct` section in the document

Cores, memory, network, `onboot`, extra features: PVE already stores and edits
these well, and a user changing them in the guest's config must simply work.
The wrapper settings this tool owns are *derived*, not configured: the two
features docker needs (`nesting`, `keyctl`), the managed volumes, the
`compose` tag. The plan adds what is missing and reports what differs; it
never removes.

## The stack directory is on the rootfs

`/opt/stack` was a disk of its own for one reason: a disposable rootfs that a
rebase or migrate could wipe or replace while the stack survived. Neither
operation exists. What remained was an extra disk per guest, two document
keys, a config key and a second place for data to live. Now only `x-pve`
volumes get disks, which is the part that matters (their storage, size and
backup flag), and plain volumes and `.env` sit on the rootfs, which is backed
up anyway.

## No guessed storage; ask once, remember

Storage names are per node and `local-lvm` exists only on a stock install, so
a built-in default was a guess that failed on a real cluster. There is none.
`new` settles the template storage and the disk storage from a flag, from the
stored choice, or from a numbered question at the terminal, in that order,
and offers to store an answer; every question is asked before anything is
done, so an unanswered one leaves nothing behind. Without a terminal it stops
and names the `config set` command. The daemon never asks: an `x-pve` disk
without a storage on a node without a stored one is refused in the plan with
that command in the message. The stored choices live in
`/etc/pve/pve-compose.cfg`, written only with what differs from the defaults
and re-read before every save so two nodes writing it do not clobber each
other.

## Volumes: identified by path, grow-only, never deleted

A managed disk is the mountpoint whose `mp=` is `/opt/stack/volumes/<name>`.
Indexes are allocated at execution time and mean nothing. A size increase is a
`pct resize`; a decrease, a storage change, or a volume vanishing from the
document is a note in `diff` and `apply`, and the disk stays. Deleting data is a
human's `pct set --delete` plus a deliberate destroy.

Every named volume, `x-pve` or not, is bound to a directory under
`/opt/stack/volumes/`, so a wrapper rebuilt from nothing keeps them all. The
rendered compose file replaces each volume's definition with a `local` volume
bound to that directory; docker's own volume semantics (copying an image's
initial content into an empty volume) do not apply to binds, which is
documented rather than worked around.

## Rendering, not overriding

The compose file in the guest is the document's `spec` with the volume binds
substituted and `${PVE_*}` expanded. No override file, no generated env file:
`cd /opt/stack && docker compose up -d` is exactly what the tool runs and what a
human can run. `.env` is the user's and is never read or written by the tool.

## No migrate verb

"The same stack on a fresh wrapper" was built three times in one day and
removed. In-place rebase (wipe the rootfs, unpack the template into it with
PVE's own `restore_archive`) worked and was only safe on a rootfs known to hold
nothing. Reassigning the data volumes to a new container (`pct move-volume
--target-vmid` on `unused` volumes) worked and left the old guest with nothing
to revert to. Copying them with `PVE::LXC::copy_volume` worked and was the
right mechanism, and the verb around it kept growing options: storage, config
carry-over, extra interfaces, raw `lxc` keys. A stack moves a few times a
year; the manual recipe in the README is six commands. `docs/ROADMAP.md` keeps
what was learned for when the arm64 node makes it worth having.

Two things that looked simpler and were not, recorded so nobody tries them
again on a guest that matters:

* `pct restore --force` over an existing container **destroys every volume of
  the old container**, including data volumes passed by volid on the command
  line, before it finds out whether the archive is usable. Verified on a lab
  container; it deleted three volumes and then failed.
* `pct move-volume --target-vmid` refuses `rootfs` at either end, and a rootfs
  cannot be detached into an `unused` volume. So a rootfs never moves between
  containers.

## Docker comes from get.docker.com, at any point; no prebuilt template

`provision::ensure_docker` is idempotent and runs on `new` and on `docker apply`
when docker is missing. A docker-ready template was built
and measured: it saved about ninety seconds on `new`, an operation
that happen a few times a year, and cost a 300 MB artifact with a refresh
schedule. It was removed. The upstream Debian template is the only input.

## One loop per node, acting only on its own guests

PVE keeps a guest's config under exactly one node, and `pct` works only there.
The daemon lists its node's containers and never looks elsewhere, so N nodes
never overlap and a migration moves the responsibility with the guest. A manual
verb given a vmid on another node re-runs itself there over PVE's own inter-node
SSH (`PVE::SSHInfo` builds the command), so the per-guest lock is always taken
on the owning node, where the daemon takes it too.

The loop is level-triggered off `GET /meta/version`: any change anywhere runs a
pass over the guests whose document digest is not the one last handled, and
every `full_every` seconds the memo is dropped and every guest is planned
again, which is what catches a change made outside the store. A guest whose
apply failed is remembered like a success, so a broken stack is retried on the
next document change or the next full pass, not every poll.

The loop is single-threaded, one guest at a time. A slow apply on one guest (a
first docker install takes two minutes) delays the others on that node for that
long. Acceptable at this scale, and nothing interleaves.

## The loop applies changes; it never starts a stack

Two different things hide in "docker apply": landing a document change in a
running stack, and starting a stack. The loop does only the first. When a
selected guest's stack is up (some container of it running), a changed document
is rendered, pushed and `compose up`'d. When it is down, whether never started
(`new --no-up`) or taken down by hand, the file is rendered and pushed so the
pending state lands, the facts record that, and nothing starts. A hand-run
`docker apply` always brings the stack up. So a manual `compose down` sticks,
and `new --no-up` needs no policy change to stay down.

## Stopped guests are never touched

Attaching a disk to a stopped guest is possible; pushing a file and running
compose is not, and half an apply is worse than none. `new` starts the guest
itself.

## Features need a restart, and apply says so

The `nesting` and `keyctl` features docker needs only take effect when the
container restarts, and docker cannot start without them. An apply that had to
set them stops after the pct half with that message, and says so on the error
path too. Nothing reboots a guest; `docs/ROADMAP.md` has the open question.

## Nothing from a document reaches a shell unvalidated

`stack.owner` and every `x-pve.owner` end up inside `sh -c` lines run in the
guest, and the hostname becomes the compose project name. Owners are validated
when the document is parsed (`uid:gid`), the project name is normalised to
compose's charset, so a holder of a scoped pve-meta
token, a lower privilege than the guest's PVE config, cannot turn a document
into a root shell there. Volume names are checked the same way.

## What pve-meta could grow, noted while building this

* A compose volume map entry with no body (`cache:`) is a YAML null, which
  pve-meta refuses. `cache: {}` is the spelling. Fine, but every compose author
  will hit it once.
* Dotted map keys (`driver_opts: { com.docker.network.bridge.name: br0 }`) fail
  pve-meta's key charset. The list forms of `labels` and `sysctls` avoid it;
  `driver_opts` has no list form. Allowing dots in keys, with slash-form
  addressing for such keys, would remove the limit.
* The daemon reads documents through the `pve-meta` CLI (`--format yaml`) to
  get exact types; the JSON API's `1`/`0` booleans would need a schema to
  round-trip into compose, which validates some fields as real booleans.
