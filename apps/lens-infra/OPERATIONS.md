# Local qualification and operation

Tracking: [local infrastructure project](https://linear.app/wildfoundry/project/lens-local-infrastructure-control-9b009d4b4cd7).
This extension is experimental. The deployment target for qualification is disposable
infrastructure; installation on the live build hosts is a separate, reviewed pilot.

## Contract

The host administrator owns policy, images, networking and credential bindings. CI selects
an approved environment, network, class, template and replica count. A release name is an
audit identifier; changing image bytes requires a new pinned template, not just a release
name. Versioned classes/templates/networks/build profiles are immutable. Policy revisions increase.

All requests name a host and its exact active policy revision. A configuration digest may
also be included. A stale request fails locally. The Unix socket authenticates the actual
local UID. SSH is a transport to that socket, with a dedicated forced-command account and
an environment-scoped role. There is no TCP management API, outbound controller session,
Dataplicity database dependency, AWS dependency or change to the reporting diode.

Existing objects are first-class **protected inventory**, with their configured capacity
charged before new builds. They are not silently adopted. Docker/LXD objects whose capacity
cannot be measured need explicit `protected_resources."runtime:identity"` reservations in
administrator policy. Missing inventory samples never release reservations. Removing a
managed VM retains its disk and charges that storage until a separately reviewed archival
procedure exists. This version deliberately has no disk purge operation.

An administrator may explicitly list a protected VM's UUID in `parked_resources` after
stopping its owning controller and gracefully shutting it down. Compute reservations are
released only while inventory freshly confirms that the VM remains defined and shut off;
its disk stays charged. A running or missing parked VM blocks capacity-increasing work
and recovery; graceful shutdown remains available.
CPU overcommit is separately bounded by administrator policy (`cpu_overcommit_percent`,
default 100). Memory retains its physical host reserve; CI cannot change either budget.

On our R630s, `wf-runner@SLOT.service` owns recycling. A direct guest shutdown triggers that
controller's normal discard/rebuild cycle. Use a per-slot hold before shutdown; do not treat
these VMs as unmanaged guests. Extending that runner controller behind a Lens adapter is
the selected integration direction. Its runner lifecycle and shared capacity cooperation
must be qualified before automatic runner scaling is enabled.

The shared `/etc/wildfoundry-runners/boot.lock` must be a root-owned regular file with
mode `0600`. When integrating an existing controller, tighten permissions on that inode
in place; do not replace an active lock file. The adapter opens it read-only so it can
coordinate through Linux flock while the service keeps `/etc` read-only.

## Qualification order

1. Inventory existing libvirt VMs and host container runtimes using `inventory --direct`.
   Record protected resources in a private state directory. Keep inventory evidence out of
   public source control. Confirm host hardware, physical memory, disk free space, routes,
   network names and configured VM reservations. Do not infer free capacity from idle RAM.
2. Run the safety suite, real HAProxy/Unix peer-identity tests and private network namespace
   packet tests. Build the separate package and confirm it contains no service-enabling
   maintainer script. It must not change the existing Lens binary or reporting services.
3. Prepare an approved self-contained image offline, with Python 3.12+, qemu-guest-agent,
   nftables, systemd and workload dependencies already installed. OCI images also need
   Docker; runner images need the pinned runner's distribution dependencies and build tools.
   Do not put customer secrets or host bootstrap keys into a golden image. Check its digest.
4. Use `scripts/qualify-infra-vm-linux.py` with unpacked distribution tools and a verified
   Ubuntu cloud image inside **both** disposable mount and network namespaces. The script
   refuses the host namespaces. It creates and removes only its own disposable test VMs.
   Qualify delivery, readiness, ingress, replacement, drain and history with a dummy secret.
5. Before a live pilot, present the exact host capacity and proposed network/package changes
   to the operator. The R630 inventory found substantial existing reservations, so this step
   needs an explicit capacity allocation. No existing container may be deleted. A potentially
   breaking runner/network change must be communicated before execution.
6. Qualify host reboot, controller restart during a deployment, artifact-service outage,
   runner registration/job completion, certificates and real VLAN/cross-host isolation on
   the agreed pilot. Web management remains outside this local milestone.

## Recorded qualification

On 2026-11-08, the Linux suite passed 86 unit/integration/real-PTY checks. Real packet tests
passed the 10 local isolation checks and the nested-host VXLAN checks, including two VNIs,
the 1350-byte MTU boundary, environment/management denial, unapproved transport peers and
guest DHCP-server spoofing. Link verification reads the kernel's read-only rtnetlink API:
older Ubuntu iproute2 builds can emit invalid detailed VXLAN JSON against newer kernels.

The real KVM/libvirt slice on an Ubuntu 26.04 ASUS test host passed creation/replacement,
local dummy-secret delivery, HAProxy, Lens/logging installation, named-user SSH and sudo
separation, private-home permissions, interactive/root prompts, offline CPU/RAM/disk growth,
backup, new-UUID stopped restore and restored boot. Actual CPU/RAM definitions and guest
filesystem growth were checked; customer/home marker files survived, and verified guest
artifacts were reused. This was inside disposable mount/network namespaces, not an R730
qualification. The separate package was audited for root ownership, directory permissions,
required shell assets and absence of service-enabling maintainer scripts.

The installed vanilla-Ubuntu pilot also completed VM builds through the restricted SSH
CI account on both physical R630 hosts. Both hosts passed graceful shutdown, offline
RAM/disk growth, standalone offline backup and reboot. Guest checks confirmed actual
RAM/filesystem growth, local configuration version, named-admin SSH/sudo, private homes,
Lens/logging installation and secret file mode `0600`. Protected inventory and stale policy
requests were rejected; local monitoring remained available without a web component.

On the first R630, a local web service completed health-gated deployment and replacement
with independently supervised HAProxy. Direct HTTP checks passed; restarting only the new
control daemon produced zero failures across 99 HTTP probes while ingress stayed active.
The recorded original parked VM remained defined and shut off on each host, with its disk
preserved; the other 13 runner-controller services per host retain the original controller.
This is a bounded pilot, not a completed host-reboot or cross-host qualification.

Installed-service testing caught admission-lock access on read-only `/etc`, directory
traversal under the private service umask, named CPU expansion and stale parked-capacity
observations. Secret files are explicitly created `0600` before delivery; the guest agent's
umask is not trusted. Lifecycle admission refreshes inventory, and graceful shutdown is
allowed even if current reservations exceed the configured budget. The optional package
is `0.3.11+infra7`; the main collector remains the existing release. The pilot controller
was started explicitly and has not yet been enabled for host boot.

These results do not close the physical R630 network, real GitHub job, reboot/failure,
certificate rotation, redundant ingress, independent rescue, IPAM/failover or fenced
migration gates. Keep the Linear project open through the internal pilot.

## Explicit bootstrap

These commands are a runbook, not an instruction to apply the example to a live host.
Installing dependencies can start distribution services; review that change first.
The package is separate and disabled by default, with no automatic post-install start.

```sh
lens-infra config validate /etc/lens-infra/host.toml
lens-infra inventory --direct --record /var/lib/lens-infra
lens-infra network plan /etc/lens-infra/host.toml
```

After reviewing the concrete host/network changes, the local administrator creates the
`lens-infra` system group, the storage directory `/var/lib/libvirt/images/lens-infra`, and
private credential files, then explicitly provisions the declared new networks and policy:

```sh
sudo lens-infra network apply /etc/lens-infra/host.toml --yes
sudo lens-infra config apply /etc/lens-infra/host.toml --local-state /var/lib/lens-infra --yes
sudo systemctl daemon-reload
sudo systemctl enable --now lens-infra.service
```

Managed guest VMs are not configured for libvirt autostart. At controller startup, the host
first verifies approved network ownership and reinstates only its own scoped nftables
tables, then starts recorded current VMs, delivers ephemeral credentials again and rebuilds
ingress. Failure to establish that protection prevents VM recovery. Existing VM autostart
and existing host firewall tables are not rewritten.

HAProxy uses independent systemd units so a controller restart does not stop load balancing.
For the first implementation, the service frontend binds loopback. An existing locally
administered edge proxy can expose it; public edge routing and a redundant ingress VM pair
remain pilot work. Local combined PEM files may terminate TLS; their key remains local.

## Human and machine output

Normal CLI commands render terminal summaries, following Lens's magenta/cyan palette.
States always have text labels; narrow terminals use stacked fields and preserve exact
selectors. Output has no ANSI colour when piped or when `NO_COLOR` is set. `TERM=dumb`
and non-Unicode output streams use ASCII.

For scripts parsing CLI results, explicitly add `--json` (before or after subcommands):

```sh
sudo lens-infra status
sudo lens-infra status --json
sudo lens-infra operation status web-build-one --json
```

The JSON envelope and failure exit codes are unchanged. Restricted SSH `rpc` always uses
JSON for results/errors. `metrics` remains Prometheus text. This display change does not
alter authorization, requests, policy, scheduling or workload lifecycle.

## CI interface

```sh
lens-infra deploy plan deploy.toml
lens-infra deploy prepare deploy.toml
lens-infra deploy apply deploy.toml --yes
lens-infra operation status web-build-one
lens-infra status
```

`prepare` verifies and snapshots local artifact bytes. `apply` commits an idempotent operation
and capacity reservation before VM creation. Disconnection of CI does not cancel it. A
controller restart leaves incomplete work interrupted; use an authenticated explicit
`operation resume ID --yes` after inspection. Cancellation retains VMs and reservations.

The fixed pipeline creates a replacement, delivers credentials, requires three readiness
passes, tests the selected protocol, introduces a small canary, observes it, switches
traffic, observes again, promotes the release and drains old backends. HAProxy never
replays a failed JSON-RPC POST. Before promotion, failure restores the previous retained
release. After promotion, old-VM cleanup failures leave the healthy new release active.
Failed replacement VMs remain charged and available for local inspection.

```sh
lens-infra service scale scale.toml --plan
lens-infra service scale scale.toml --yes
lens-infra service drain selector.toml --plan
lens-infra service rollback selector.toml --plan
lens-infra operation cleanup old-operation-id --yes
```

Service changes require the expected current operation ID, host and policy revision.
Scaling uses a full replacement with surge reservations, rather than in-place resize.
Drain makes the desired replica count zero. Rollback creates fresh instances from the
previous approved template, subject to current policy and health checks. Cleanup refuses
the active release and all protected resources, and never deletes host containers or disks.

Status exposes host/configuration identifiers, desired/current/previous operations,
resource state, health samples and secret **generation numbers**. `lens-infra metrics`
exports Prometheus text through the same authenticated local interface. Audit events include
actor UID and stages. Values and raw guest/native process output are not returned.

## Workload sources and application shim

OCI references may use ECR, GHCR, Docker Hub or another OCI registry, pinned as
`registry/repository@sha256:...`. A verified local archive may supply the same image offline;
declare both `archive_sha256` and its Docker `image_id` (`sha256:...`). Docker archives do not
reliably preserve a registry reference, so the pinned local image ID is checked after loading.
A container's optional `registry_secret` names a declared local secret containing Docker
authentication JSON; it is delivered to a private `/run` Docker config, never to argv.
Prepare archives on each target before a deployment that must survive registry downtime.

An application and an optional protocol shim are declared as fixed containers in one
approved VM template. They share guest loopback. The guest firewall exposes only the
approved service port, so peers cannot bypass the shim through another application port.
The runtime grants neither host mounts nor privileged host containers. The shim owns
application-specific readiness/drain behavior; the controller owns rollout and ingress.
Every exposed profile needs an HTTP 200 health endpoint. WebSocket profiles additionally
need an upgrade/ping-pong endpoint. JSON-RPC profiles need a harmless health method
(default `lens.health`) returning `true` or `{"ready":true}`. No business RPC is probed.

## Persistent VMs, users and disk recovery

Persistent servers use a different lifecycle from replaceable application services:

```sh
lens-infra vm create build.toml --plan
lens-infra vm create build.toml --yes
lens-infra vm shutdown instance.toml --yes
lens-infra vm resize resize.toml --plan
lens-infra vm backup instance.toml --yes
lens-infra vm backups
lens-infra vm restore restore.toml --plan
```

A build selects `operation_id`, `host_id`, `configuration_revision`, `environment`, `name`,
`class`, `template`, `network` and `release`. `start = false` creates a defined, stopped VM.
An instance selector names `operation_id`, `host_id`, `configuration_revision` and the exact
`instance_id`; resize adds `new_class`. The controller derives all storage paths. Persistent
servers have private ingress and cannot be overwritten by a stateless service deployment.

Shutdown records desired power off before ACPI shutdown; controller recovery respects it.
Resize requires an explicit shutdown and confirmation that the guest is actually off.
Disk shrink is rejected. Growth and CPU/memory changes reserve the larger allocation before
changing the disk or definition; guest filesystem size is observed after the next boot.
Backup requires the guest off and makes a standalone, checked, hashed disk copy. Backups
continue to count against storage capacity. Delete requires shutdown and retains the disk.
Online snapshots, automatic backup schedules and disk purge are not implemented.

A restore selector supplies `operation_id`, `host_id`, `configuration_revision`, `backup_id`,
`name`, `class` and `network`. Restore requires an environment-scoped `operator` role or local
root; a CI `deployer` cannot restore disks or boot a restored copy. It creates a new UUID and
MAC, private and stopped, with a separate explicit boot operation. Disk contents include the
original home directories and application identity: review identity/credential duplication
before booting a recovered copy alongside its source. This is recovery, not migration.
Interactive console/rescue access remains a separately authorized feature to qualify.

An approved template may select a versioned `build_profile`. Its `lens` package requires a
local path, SHA-256 and exact package version; `logging` requires a local path and SHA-256.
The guest checks both bytes and package identity, validates rsyslog configuration and starts
only guest services. Lens reporting is not enrolled automatically. Profiles and images are
cached locally; Dataplicity web is not involved.

Named human accounts belong in the approved build profile:

```toml
[[build_profiles.customer-v1.users]]
name = "alice"
ssh_keys = ["ssh-ed25519 REPLACE_WITH_ALICES_PUBLIC_KEY"]
sudo = true

[[build_profiles.customer-v1.users]]
name = "bob"
ssh_keys = ["ssh-ed25519 REPLACE_WITH_BOBS_PUBLIC_KEY"]
sudo = false
```

Replace the example key text with actual public keys before validation. Each user gets an
owner-only local `/home/<name>` on the VM's disk and a separate primary group. Administrators
receive passwordless sudo; ordinary users do not. Root SSH, password SSH and keyboard password
authentication are disabled. No root password or private user key enters the build request.
Persistent server builds require at least one administrator public key. The simpler legacy
`ssh_keys` profile field creates one bootstrap `vmadmin` account; it cannot be combined with
named users.

Home directories are **not centrally mounted**. They follow that VM's disk backup/restore
and eventual migration. Later central management can publish user/public-key/role definitions
without making logins depend on central storage. Shared project/data volumes should be
explicit resources; this version has no shared-home or arbitrary host-mount interface.
Disposable runners should keep useful state in deliberate caches/work volumes, rather than
expecting their home directories to survive normal runner recycling. Build profiles apply
accounts at creation; fleet account/key reconciliation and revocation are separate work.

### Headless server experience

Set `shell = "dataplicity-server-v1"` in the selected build profile to install the server
shell and `dp-status` in that guest. A normal prompt has two lines:

```text
[PRODUCTION] alice@api:5c6a9f5d ~/project
$
```

The environment is always text, with an additional red production accent on colour
terminals. Elevated shells add `ROOT` and use `#`; a failed command adds its exit code.
Narrow terminals put the working directory on its own line. ASCII terminals and `NO_COLOR`
work without special fonts. Context comes from a small local data file; prompt rendering
uses Bash builtins and does not call the network, Git or the controller. Control characters
in paths are neutralized, and metadata/path strings are never executed as shell code.

The welcome summary reports the applied VM class, instance configuration revision and build
profile, then fresh local system state, load and root disk use. These are system readings,
not a claim that every customer application is healthy. `dp-status --json` is available to
local scripts. Welcome text appears only for an interactive terminal; SSH command mode,
SCP and CI receive no shell banner. Existing prompt hooks and command exit status survive.
Set `DP_SHELL=plain` for a plain prompt, or `DP_SHELL=off` before the managed `.bashrc` line
to keep a personal prompt. Installation adds that line once; recovery does not reinsert a
line the user later removes.

The active target is vanilla Ubuntu using standard distribution dependencies and the
optional infrastructure package. The server shell and Lens/logging profiles are ordinary
installable components. Custom Ubuntu images, installer changes and Faceplate integration
are deferred to the separate DAT-858 task; none is a dependency or acceptance gate here.
Use the existing Ubuntu installation and local configuration on our hosts. Guest templates
can start from verified standard Ubuntu images with the required workload packages
provisioned locally. Named local access and recovery must work without web access.

## Software fabric and future VM moves

`mode = "vxlan"` networks require a managed `li-` network, unique `vni` and a fabric listing
each host's stable ID, private transport endpoint and unique `node_id` (1 through 32).
Declare every management/transport range in `protected_cidrs`. The endpoint must already
exist on the physical underlay, and peer routes must use it; Lens does not reconfigure the
physical NICs, switch uplinks or existing runner bridges. Cross-site transport needs an
encrypted underlay; VXLAN itself does not encrypt customer traffic.

The first fabric uses one explicitly selected `gateway_host` for DHCP/NAT, with a conservative
MTU of 1350 (configurable 1280 through 1450). Network addresses .1 and .2 through .33 are
reserved for the gateway and fabric nodes; guest DHCP starts at offset 34. Other hosts use
owned software bridges connected by VXLAN. The firewall permits only declared transport
peers, blocks guest access to protected physical networks and UDP 4789, and drops guest VLAN
tags, DHCP-server traffic and IPv6 router advertisements. Environment isolation remains
enforced even when a network permits general internet egress.

Apply network changes only with the local executor stopped. Active network definitions
cannot be removed, and active gateway/MTU changes or member removal require a future
maintenance/fencing operation. A newly added member requires an explicit reviewed policy
revision and network reconciliation on the existing hosts. The single gateway is not HA;
durable IPAM, gateway failover and physical two-host qualification remain acceptance gates.

An optional `migration_pool` pins a pool `name`, compatible `cpu_model` and exact QEMU
`machine`, for example `Haswell-v4` and `pc-q35-8.2` after checking each member. Guest MAC
addresses derive from permanent VM UUIDs and DHCP identifies the MAC. These are migration
foundations; this version has no host-move command. Planned moves must reserve the destination,
preserve UUID/MAC/disks/metadata, transfer required local secret bindings and fence the source
before destination boot. Live migration and automatic failover remain separate work.

## GitHub runner lifecycle

A `github-runner` template supplies a verified local runner archive, GitHub organization or
repository URL, explicit labels, group and a locally bound registration token. Automatic
default labels are disabled; declare `self-hosted`, OS and architecture labels explicitly
if existing workflows should select these machines. It uses a VM, not an
ECR container requirement. Registration tokens are passed through the upstream runner's
`ACTIONS_RUNNER_INPUT_TOKEN` environment input and excluded from native process arguments.
Pinned distributions disable automatic updates: the operator must publish a new template
before GitHub's runner update deadline.

The registration environment input is defined by the [upstream runner implementation](https://github.com/actions/runner/blob/main/src/Runner.Listener/CommandSettings.cs).
Ephemeral lifecycle expectations follow [GitHub's runner documentation](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#ephemeral-runners-for-autoscaling).

New runners are ephemeral by default. A successful one-job exit records a durable completion
marker and will not register again on recovery. Scaling provisions approved capacity; this
version does not poll GitHub queues or provide a scale-set adapter. Persistent runners are
an explicit template choice. During replacement or drain, a busy runner or one still
accepting jobs remains `drain-pending`. An idle snapshot is insufficient for removal.
After ephemeral completion, or after an operator stops admission locally and the job exits,
explicit old-operation cleanup can unregister the VM. Unadopted runner services are never
stopped or migrated by this implementation. GitHub-side deletion/revocation stays manual.

## Secrets and recovery

The customer creates versioned local credential files. Encrypted bindings use
`systemd-creds` with the correct credential name and host-local key/TPM policy; a private
root-owned plaintext file is an explicit alternative. Configuration stores only path,
generation and allowed environments. Decrypted values travel through the native guest
agent into `/run/lens/secrets`, outside VM images, controller journals and command arguments.
Guest root, the host administrator and authorized workload code remain trusted.

Rotate by creating a new local file and increasing its generation in a new policy revision,
then making a health-gated replacement deployment. Keep previous versioned files while
rollback/recovery may need them. To add a host, provision its local decryption material and
approved secret bindings through a customer-controlled channel. Removing a host from
scheduling does not revoke credentials automatically. Revocation remains a separate manual
action. This local version does not escrow or distribute customer secrets through Dataplicity.
