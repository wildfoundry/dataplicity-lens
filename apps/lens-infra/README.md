# Local Lens infrastructure extension

Implementation tracking: [DAT-845](https://linear.app/wildfoundry/issue/DAT-845).
Setup, CI examples, recovery and the qualification plan: [operator runbook](OPERATIONS.md).

This is an experimental, separately installed Linux extension. It does not modify the
reporting collector, publisher or one-way FIFO. No web integration exists or is required.
The current deployment target is vanilla Ubuntu with normal distribution dependencies,
the optional controller package and local configuration. Custom OS images, installers
and Faceplate integration are deferred to a separate task and are not prerequisites.
Do not enable it on production hosts until the hardware, recovery and security gates in
the Linear project are qualified.

## VPS experience

The product is a straightforward VPS connected to the Dataplicity ecosystem: Lens,
monitoring, logging, useful local access and recovery, with optional IoT/shim integration.
The first-boot experience and useful operational information are the priorities. A plain
server does not require an application container, an HTTP endpoint or a shim. Custom OS
builds remain a separate task; the first integrated guests use vanilla Ubuntu packages.

For the future internal web interface, use a direct **Servers** entry with **Create VPS**
as the primary action. Show location, environment/network, OS image, available sizes and
SSH access together. The resulting server page should make its address, current state,
configuration revision, health, logs and recovery controls easy to find. Application
deployment is a further action with an explicit workload definition, not a prerequisite
for creating or understanding a server. Keep unavailable sizes visible with concrete
capacity reasons. The API must enforce the same internal feature gate as the navigation.

Keep the build choices distinct:

| Choice | Purpose | Example |
| --- | --- | --- |
| VM image | Approved, cached base OS disk, pinned by checksum | `ubuntu-server-v1` |
| Class | CPU, RAM and disk allocation allowed on this host | `small-v1` |
| Build profile | Named users, Lens/logging, files and run-once setup | `connected-v1` |
| Workload template | Optional application/runner behavior and ingress | Private server, web service, GitHub runner |

An image name is a local catalogue key, not an AWS identifier. The administrator registers
the disk path, checksum and supported capabilities once; a build explicitly selects that
name. New image bytes require a new name. Different operating systems can be approved;
the selected profile's dependencies must be satisfied. The current Lens package installer
requires a Debian-family guest. Container/OCI image digests are a separate choice inside
an application template and never stand in for the VM's base OS.

CI prepares application artifacts. A versioned workload definition tells the guest runtime
what to execute: pinned application image, command, limits, ports, storage, secret bindings,
health and shutdown rules. This is the useful task-definition/service distinction in ECS.
File injection and per-VM run-once setup do not replace supervision of long-running services.
Database migrations need a separately coordinated deployment job; they must not run once
per replica simply because a new VPS is bootstrapping. The full worker/scheduler, storage
and release-publication contract remains in progress under DAT-865 and DAT-868.

```sh
lens-infra images
lens-infra classes --environment lab --template server-v1 --image ubuntu-server-v1
lens-infra vm create build.toml --image ubuntu-server-v1 --plan
```

These are example names; use the catalogue on the target host. No OS is selected implicitly.
This source adds the explicit image contract; migrate host policy and existing CI examples
before upgrading an older installed controller. Central web management is tracked separately
in [DAT-868](https://linear.app/wildfoundry/issue/DAT-868).

## Current implementation

- Administrator-owned TOML policy and exact configuration revisions.
- Approved VM classes, workload templates, environment networks and local secret bindings.
- Durable SQLite operations, audit records, capacity reservations and idempotent requests.
- Existing libvirt VMs and discovered Docker/LXD objects registered as **protected resources**.
- VM reservations use configured memory and vCPU allocations, including stopped guests;
  low instantaneous memory use does not imply unallocated capacity.
- Local Unix-socket requests authenticated from kernel peer credentials, plus restricted SSH transport.
- Replacement VM lifecycle through the existing local libvirt backend; guest operations use
  native libvirt bindings so secret payloads never enter native process arguments or virsh history.
- HAProxy configuration generation for fast web, long-lived WebSocket and JSON-RPC profiles.
- OCI images from any registry, pinned by digest, with optional verified local archives.
- GitHub runner templates using a pinned local distribution archive and a local registration-token binding.
- Plan/apply scaling, drain and fresh-build rollback guarded by the expected current release.
- Local health observation, canary ingress and protocol checks, with independent ingress supervision.
- Passive local threshold evaluation for sustained VM/container pressure and HTTP endpoint failures.
- Current class availability with explicit capacity, authorization and preparation shortfalls.
- Locally administered environment network creation and scoped nftables rules.
- Explicit approved base-image selection, pinned file injection and recorded per-VM run-once setup.
- Persistent VM builds, graceful power controls, guarded offline resizing and disk backup/restore.
- Verified pre-resize local recovery copies, retained for seven days and charged to capacity.
- Versioned Lens/logging build artifacts and named SSH users with private local home directories.
- Explicit software VXLAN fabric membership and pinned CPU/machine compatibility pools.
- An opt-in adapter for the existing custom Wildfoundry runner controller, with durable per-slot hold/resume.

Existing resources remain protected inventory unless a local administrator explicitly
adopts a supported runner slot. The runner adapter has its own job-aware hold/resume path;
ordinary VM controls cannot resize or delete those runners. There is no operation to delete
host containers. VM destruction checks
the persistent reservation, UUID and Lens ownership metadata before acting. Retained disks
are kept for recovery and must continue to count against storage capacity.

## Terminal output

Commands render human-readable summaries by default: Lens-coloured headings, service
releases, VM sizes/states, protected inventory, plans, operation progress and errors.
Tables adapt to terminal width; narrow terminals use stacked fields without losing UUIDs.
Pipes and `NO_COLOR` omit colour, and dumb/non-Unicode terminals use ASCII.

```sh
lens-infra status
lens-infra operation status web-build-one
lens-infra status --json
lens-infra --json operation status web-build-one
```

`--json` preserves the versioned envelope for scripts and is accepted before or after
subcommands. Restricted SSH `rpc` always emits JSON, including errors, without needing
that flag. `metrics` continues to emit Prometheus text. Existing scripts that parse the
ordinary CLI's previous default JSON must add `--json`.

## Development

Requires Python 3.11 or newer. The guest workload runner requires an approved Linux VM
image with Python 3.12+, qemu-guest-agent and the workload's runtime/dependencies already installed.
The Python extension uses the standard library except the distribution's native libvirt
bindings for guest communication. It is packaged separately so normal Lens inspection
keeps its existing native binary and dependency contract.

```sh
PYTHONPATH=apps/lens-infra python3 -m lens_infra --help
PYTHONPATH=apps/lens-infra python3 -m unittest discover -s apps/lens-infra/tests -v
```

Read-only inventory works with the invoking account's existing permissions and does not
require the daemon:

```sh
PYTHONPATH=apps/lens-infra python3 -m lens_infra inventory --direct
PYTHONPATH=apps/lens-infra python3 -m lens_infra inventory --direct --record /trusted/local/inventory-state
```

`--record` writes an owned local inventory database; it does not adopt runtime resources.

## Trust and operational boundaries

The local administrator and installed software are trusted. The CI identity may select
approved templates and classes; it cannot redefine policy, submit a shell command, select
an arbitrary host mount, replace a secret source, or claim a different UID in its request.
Use a dedicated SSH account, pinned host keys and a forced command such as
`command="/usr/bin/lens-infra rpc",restrict` on its authorised key. The account's UID must
be explicitly granted deployment permissions in policy and socket access locally.

Secret values are read only for local delivery. They are excluded from configuration,
operation history, status and audit events. They necessarily exist in the authorised
workload's memory; root and deployed workload code remain trusted. Customer-secret
enrolment, recovery and rotation are local administrative responsibilities. Removing a
host from scheduling does not automatically revoke its credentials.

The local certificate provider accepts a combined PEM certificate/key file. Endpoint keys
remain on the local management infrastructure. Public ACME and AWS adapters are not required
for local operation and are not prerequisites for the first qualification slice.

## Qualification still required before production use

This extension is not yet a complete production scheduler. The selected runner controller
is adopted incrementally, without replacing other active slots. Hardware reboot/failure
qualification, real GitHub registration/job completion, certificate rotation, physical
cross-host isolation, redundant ingress, multi-host
coordination and the internal pilot must meet their Linear acceptance gates. A unit-test
fake backend is never evidence of real VM qualification. Review the runbook for the exact
boundaries: loopback ingress, explicit host placement, replacement scaling and retained disks.

The first live inventory found both R630s already heavily reserved for build VMs. Preserve
those VMs and containers; a capacity rebalance must be reviewed with the operator before
any potentially breaking change. Never install another virtualization manager beside the
live libvirt setup just to satisfy the initial design sketch.
