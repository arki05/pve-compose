# Roadmap

Things deliberately not in the tool yet, with what was learned about each so
the next attempt starts where the last one stopped. None of these is promised.

## Migrate: the same stack on a new wrapper

The operation: a new container from the current template, the stack directory and
managed volumes copied over, the pve-meta document copied, the stack up on the
new one, the old one stopped and kept whole so reverting is "start the old,
destroy the new". It is what makes the wrapper truly disposable, and it is the
cross-architecture move once an arm64 node exists: the document is
architecture-neutral, the rootfs is not.

Built twice and removed (`docs/DESIGN.md` records why the two mechanisms went):
the surface grows fast once storage choice, config carry-over, extra
interfaces and raw `lxc.*` keys enter, and a stack moves a few times a year.
Until it comes back, the recipe is in the README under "Migrating a stack by
hand".

What a future version must get right:

* PVE has no command line for copying one volume to another guest;
  `PVE::LXC::copy_volume` (behind `pct clone --full`) is the routine, and the
  tool's reason to exist here is that it can call it.
* The config API returns raw `lxc.*` lines as one `lxc` key holding an array
  of pairs, not as separate keys.
* Everything stopped from the first step to the end; maintenance, not uptime.
* Carry only the settings the tool understands and say which; refuse the rest
  (raw `lxc`, a second interface, an `unused` volume, a mountpoint outside
  `/opt/stack`) unless a `--force` says to proceed without them, loudly.
* Prepend to the old guest's description, never replace it; free a copied
  volume that could not be attached.
* Check the images in the spec exist for the target architecture
  (`docker manifest inspect`) before a cross-arch move.

## A restart-pending check before installing docker

An apply that sets the features stops and asks for a `pct reboot`. A guest
whose features were set earlier and never restarted still runs without nesting,
and docker cannot start there; the tool does not notice and installs docker
anyway. The accurate test is whether the config PVE generated at the last start
(`/var/lib/lxc/<vmid>/config`) lacks `lxc.apparmor.allow_nesting = 1` while the
PVE config asks for nesting. A flag that reboots in that case was built and
removed until the check exists; undecided whether the tool should then reboot
or refuse to install docker until the guest has been restarted.

## Host binds and unprivileged wrappers

A bind of a host path whose files belong to some uid shows them as `nobody`
inside an unprivileged container unless the config carries an idmap for that
uid. The tool does nothing about it. Device passthrough and idmaps are
expected to become pve-meta concerns; until then a wrapper that needs them is
set up by hand.

## Actions from the PVE UI

An "Upgrade" button on the Metadata tab that runs `pve-compose upgrade
<vmid>` as a PVE task, with its log and history. The shape agreed on: a tiny
Perl API module mounted by pve-ext that forks a worker running the CLI, and a
prefix-file declaration the editor renders as a button. Not a flag in the
document: a flag has no log, sticks when the daemon is down, and a snapshot
rollback would re-fire it.

## Forgetting guest-files' records

0.0.3 to 0.0.6 wrote the stack's two files through pve-meta-guest-files,
which left `managed/compose/*` records in each guest's
`/etc/pve-meta/.guest-files`. Files are written with plain `pct push` now,
and the records are left alone on purpose (`docs/DESIGN.md`, "Files go in
with `pct push`"): nothing deletes a managed file, and guest-files is on its
way out. A one-shot removal of them is only worth writing if one turns out
to get in the way.

## pve-meta items this tool would use

* `additionalProperties` schemas, so `spec.volumes.<any>.x-pve` gets typed
  rows and validation in the editor. In the PVE::JSONSchema dialect already;
  pve-meta's core walks `properties` only.
* Dots in map keys, addressed in slash form, so `driver_opts` maps are storable.
* Documents in guest backups: done in pve-meta 0.1.2. A guest restored on a
  node with that version has its document; the tool does not depend on it.
