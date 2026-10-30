# Installing Dataplicity Lens

Installation does not start or enable a reporting daemon, register a host, or transmit observations.
Any future reporting service must remain off until explicitly enabled; see the
[fleet reporting design](FLEET_REPORTING_DESIGN.md). Hardware identity reads use your existing
permissions. A restricted serial is shown as unavailable, without requiring Lens to run as root.

Lens is built first for Linux systems in the field, including Raspberry Pi OS, Debian and Ubuntu.
macOS is also supported for local development and support work. Run Lens as your normal user; only a
specific systemd action may ask for authorisation under the machine's existing policy.

## Raspberry Pi OS, Debian and Ubuntu

Download the Debian package for the machine from
[GitHub Releases](https://github.com/wildfoundry/dataplicity-lens/releases). Check the
architecture on the target itself:

```sh
dpkg --print-architecture
```

Choose the package with the matching suffix:

| Output | Package | Typical systems |
| --- | --- | --- |
| `armhf` | `dataplicity-lens_<version>_armhf.deb` | 32-bit Raspberry Pi OS |
| `arm64` | `dataplicity-lens_<version>_arm64.deb` | 64-bit Raspberry Pi OS and ARM gateways |
| `amd64` | `dataplicity-lens_<version>_amd64.deb` | Intel/AMD Debian and Ubuntu systems |

Then install the downloaded package and open the live overview:

```sh
sudo apt install ./dataplicity-lens_<version>_<architecture>.deb
lens
```

Verify the download before installation. `SHA256SUMS` is published beside every release artifact:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

On macOS use `shasum -a 256 -c SHA256SUMS`. The manifest must report the selected package or archive
as `OK`; do not install a file that is absent from the manifest or has a different digest.

This is a local package install through `apt`; it resolves package requirements and registers Lens
with the system package database. The `armhf` build targets the hard-float ABI used by 32-bit
Raspberry Pi OS. All nine commands and their manual pages are installed under `/usr`.

For a quick non-interactive check on an unattended device:

```sh
lens-top --once
lens-health --json
```

Upgrade by downloading the newer package for the same architecture and installing it through `apt`:

```sh
sudo apt install ./dataplicity-lens_<new-version>_<architecture>.deb
lens-top --version
```

The package replaces all nine commands together so mixed command versions cannot remain installed.
Remove it with `sudo apt remove dataplicity-lens`. Lens does not create a daemon or persistent data
store; removing the package removes the installed binaries, manual pages and completions.

## Other Linux systems

Debian and RPM packages use statically linked binaries so they also work on systems with an older
glibc, including current Raspberry Pi OS and Debian releases. Releases additionally include GNU and
statically linked musl archives for x86-64, ARM64 and ARM hard-float. Verify the selected artifact with
`SHA256SUMS`, unpack it, and install the binaries in `/usr/local/bin`. Archive installations are not
registered with a package manager, so record the installed version and remove or replace all nine
binaries together during an upgrade.

## macOS with Homebrew

The WildFoundry tap installs all nine applications, their manual pages and shell completions from a
checksummed CI-built release archive. Apple Silicon and Intel Macs receive their native build.

Prerequisites:

- macOS on Apple Silicon or Intel
- [Homebrew](https://brew.sh/)

Install and open Lens:

```sh
brew tap wildfoundry/tap
brew trust --formula wildfoundry/tap/dataplicity-lens
brew install wildfoundry/tap/dataplicity-lens
lens --version
lens
```

Homebrew verifies the archive digest before installing it. Current Homebrew releases require
`brew trust` for third-party tap formulae before install. Rust and Apple's developer tools are not
required for this binary installation. The current published release reports `lens 0.3.7`.

Open the overview, then check the local machine:

```sh
lens
lens-top --once
lens-health --json
```

Open the interactive process explorer with `lens-top` and press `q` to exit. The other native
collectors are `lens-services`, `lens-containers`, `lens-logs`, `lens-disk`, `lens-net`, `lens-hardware`, `lens-system`
and `lens-health`.

macOS privacy controls can hide some unified-log or per-process details. Lens preserves normal user
permissions and reports unavailable information as warnings instead of requiring `sudo` or failing
the entire snapshot.

Upgrade to the newest published version or remove the suite with:

```sh
brew update
brew upgrade dataplicity-lens
lens --version
brew uninstall dataplicity-lens
```

## Build directly from source

The workspace pins its Rust toolchain. Build every command with:

```sh
git clone https://github.com/wildfoundry/dataplicity-lens.git
cd dataplicity-lens
cargo build --release --locked --workspace
./target/release/lens
```

The executables are written to `target/release/`.

Source builds are primarily for contributors. Operators should prefer a release package so the exact
build can be identified and cleanly upgraded or removed.

Contributors can test the source-build formula without publishing it by running
`scripts/test-homebrew-local.sh`; pass `--keep` to retain that temporary local installation.

See [`COMPATIBILITY.md`](COMPATIBILITY.md) for platform coverage and
[`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) when a native facility is unavailable.

## Local LXD containers and virtual machines

`lens-containers` detects LXD through its `lxc` client and reads accessible local projects. JSON includes `instance_type` (`container` or `virtual-machine`), `project`, and a project-qualified `id`. The plain view shows type/project; the interactive detail view includes both. No configured remote LXD servers are queried.

```sh
lens-containers --runtime lxd --once
lens-containers --runtime lxd --action restart --target default/example --dry-run
lens-containers --runtime lxd --action restart --target default/example --yes
```

Lifecycle commands preserve the project and target the local daemon. They do not force shutdown or delete instances. Existing LXD permissions apply; denied socket access is reported rather than presented as an empty inventory. Use an already authorized account (or sudo where appropriate); Lens does not grant LXD access. This supports LXD's `lxc` client, not classic `lxc-*` tools or Incus.

### Container and VM resources

Every supported runtime (LXD, Docker, Podman and nerdctl) has a resource summary beneath each instance in the interactive list. Press Enter for live performance charts and Tab to switch to allocations, runtime-reported usage and mounts. The interactive view samples automatically; `r` reloads the inventory. Plain and JSON output include the latest runtime counters.

LXD allocations come from expanded configuration and devices so inherited profile settings are included. Details show CPU allocation/affinity, RAM limit and usage, root-disk limit and reported capacity, storage pool and extra disk mounts. JSON exposes cumulative LXD CPU time; the interactive monitor converts counter deltas into CPU percentage. Zero disk-usage placeholders for VMs are treated as unavailable.

OCI allocations come from container inspection: CPU quota and affinity, memory limit, root-filesystem storage options and mounts. A bounded, one-shot stats query adds CPU percentage and memory usage for running containers. A stats memory denominator can be host RAM and is never treated as a configured memory allocation. Where no CPU/RAM limit is configured, the view says shared; unavailable data says not reported. Writable-layer usage, when supplied by inspection, excludes image layers and mounted volumes. Lens does not scan container filesystems to invent a disk-usage value. Runtime permissions and unsupported stats commands can limit coverage without hiding the instances themselves.

### Live container performance

The interactive container detail page shows CPU, memory, network receive/transmit and disk read/write charts. Collection runs in the background with a two-second pause between samples; slower runtime queries increase that interval. Only one automatic collection runs at a time. Each instance has up to 300 samples of session-local history, retained while browsing between instances. No monitoring daemon, guest command execution or extra permissions are required.

CPU 100% means one logical CPU, so a busy four-vCPU instance can reach 400%. Each chart scales independently to its visible peak, rather than implying a common percentage scale. Missing counters, restarts and collection gaps are shown as unavailable, not zero. The header shows sample age and collection errors. Short terminals use compact charts; Tab switches between charts and the existing allocation details.

Docker, Podman and nerdctl use their compatible one-shot stats counters where supported. Network and block-I/O counters are converted to bytes per second; these are container counters, not physical drive throughput. LXD uses guest CPU/memory and attached NIC counters, excluding internal guest bridges and loopback to avoid double-counting. The current LXD instance-state API does not report VM disk I/O, so those charts show unavailable. Root-disk allocation/usage is separate from I/O throughput. History is not persisted when Lens exits.


## Local libvirt virtual machines

Lens detects `virsh` and queries only the local `qemu:///system` connection with `--readonly`.
Use an account already permitted to inspect that daemon; denied access remains a collection warning.
Lens does not change permissions or offer libvirt lifecycle actions.

```sh
lens-containers --runtime libvirt --once
```

VM names are joined to stable UUIDs before and after each stats query so a recycled runner name
does not inherit another VM's history. CPU time and network/block counters feed the live detail
charts. Memory usage is host RSS, not guest memory consumption. Disk capacity and image allocation
refer to the first reported block device, not guest filesystem usage. Missing counters remain
unavailable. No guest commands are executed and no remote libvirt URI is consulted.
