# Installing Dataplicity Lens

Lens supports Linux and macOS. Run it as your normal user.

## macOS with Homebrew

The repository contains a Homebrew formula and a test installer that builds all seven applications,
installs their man pages and shell completions, then checks sample output and native macOS collection.

Prerequisites:

- macOS on Apple Silicon or Intel
- [Homebrew](https://brew.sh/)
- Apple's command-line tools (`xcode-select --install` if they are missing)
- Git

Clone the repository and keep the tested installation:

```sh
git clone https://github.com/wildfoundry/dataplicity-lens.git
cd dataplicity-lens
scripts/test-homebrew-local.sh --keep
```

Homebrew installs Rust as a build-only dependency. The first build can take several minutes. The
script refuses to replace an existing `dataplicity-lens` formula or an existing
`local/dataplicity-lens-test` tap.

Try the sample data first, then check the local machine:

```sh
lens --demo
lens-top --demo
lens-top --once
lens-health --json
```

Open the interactive process explorer with `lens-top` and press `q` to exit. The other native
collectors are available as `lens-services`, `lens-logs`, `lens-disk` and `lens-net`.

macOS privacy controls can hide some unified-log or per-process details. Lens preserves normal user
permissions and reports unavailable information as warnings instead of requiring `sudo` or failing
the entire snapshot.

Remove the retained test installation and temporary tap with:

```sh
brew uninstall dataplicity-lens
brew untap local/dataplicity-lens-test
```

Run `scripts/test-homebrew-local.sh` without `--keep` when you only want a clean verification; it
automatically removes the package and tap after its tests pass.

## Raspberry Pi OS

Release artifacts support both 64-bit and 32-bit Raspberry Pi OS. Check the installed userland before
downloading a package:

```sh
dpkg --print-architecture
```

- `arm64`: use the `aarch64-unknown-linux-gnu` archive or `dataplicity-lens_0.3.0_arm64.deb`.
- `armhf`: use the `arm-unknown-linux-gnueabihf` archive or `dataplicity-lens_0.3.0_armhf.deb`.

The `armhf` release targets ARMv6 with the hard-float ABI so it also runs on newer Raspberry Pi models
using the 32-bit operating system. Install the Debian package, then check both sample and native data:

```sh
sudo apt install ./dataplicity-lens_0.3.0_armhf.deb
lens --demo --plain
lens-top --once
```

## Build directly from source

The workspace pins its Rust toolchain. Build every command with:

```sh
git clone https://github.com/wildfoundry/dataplicity-lens.git
cd dataplicity-lens
cargo build --release --locked --workspace
./target/release/lens --demo --plain
```

The executables are written to `target/release/`.

## Release archives and Linux packages

Release automation produces checksummed archives for Apple Silicon macOS, Intel macOS, x86-64 Linux,
ARM64 Linux and 32-bit Raspberry Pi OS. Linux releases also include Debian and RPM packages. Verify
downloaded artifacts against `SHA256SUMS` before installing them.
