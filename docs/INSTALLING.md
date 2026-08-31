# Installing Dataplicity Lens

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
required for this binary installation. The current published release reports `lens 0.3.1`.

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
