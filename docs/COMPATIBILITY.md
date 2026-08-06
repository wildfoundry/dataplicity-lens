# Platform compatibility

Lens is designed for Linux systems in the field and for macOS workstations used to support them.
It collects what the local operating system and invoking user expose; a missing optional facility
reduces that part of the result instead of preventing the rest of the suite from running.

## Packaged architectures

| Package architecture | Typical systems |
| --- | --- |
| `amd64` / `x86_64` | Intel and AMD Debian, Ubuntu and other Linux systems |
| `arm64` / `aarch64` | ARM gateways, servers and 64-bit Raspberry Pi OS |
| `armhf` | 32-bit hard-float Raspberry Pi OS and compatible ARMv6/ARMv7 userlands |
| Apple Silicon | ARM64 macOS |
| Intel macOS | x86-64 macOS |

Linux releases contain Debian and RPM packages plus GNU and statically linked musl archives. The
native packages install the musl build so they do not inherit the GitHub build runner's glibc
version. Choose a Debian package from `dpkg --print-architecture`, not from the CPU model alone.

## Capability matrix

| Area | Linux | macOS | Minimal container behaviour |
| --- | --- | --- | --- |
| Processes | `/proc`, users, cgroups, I/O and descriptors where permitted | Native process tools and system interfaces | Namespace-visible processes only |
| Services | systemd inspection and guarded actions | launchd inspection | Empty or unavailable when no service manager is present |
| Logs | systemd journal and selected local files | Unified log and selected local files | Selected files or mounted journal only |
| Storage | mounts, capacity, inodes, block devices and deleted-open files | Mounted volumes, APFS context and device inventory | Namespace-visible mounts and devices |
| Network | interfaces, addresses, routes, listeners and ModemManager cellular data | Interfaces, routes and listeners | sysfs/procfs fallbacks when `ip` or `ss` is absent |
| Hardware | DMI/device tree, thermal zones, hwmon, USB and serial devices | Native hardware, firmware, USB and serial inventory | Only devices and sysfs mounted into the container |
| System | clock/NTP, resolver, local accounts/groups and managed certificates | Clock, resolver, local accounts/groups and managed certificates | Container namespace and mounted files only |
| Health | Evidence-backed checks over available domains | Evidence-backed checks over available domains | Never treats unavailable collection as a healthy zero |

## Required and optional facilities

The process and host collectors use operating-system interfaces that are normally present. Other
domains integrate with native facilities when available:

- `systemctl` and `journalctl` for systemd service and journal data;
- `log` and `launchctl` on macOS;
- `ip`, `ss`, `netstat`, sysfs and procfs for networking;
- `mmcli`/ModemManager for 3G, 4G and 5G modem and SIM data;
- `openssl` for certificate subject, issuer and expiry metadata;
- `vcgencmd` for Raspberry Pi firmware power and throttling status.

These are capabilities, not blanket package dependencies. Their absence appears as unavailable data
or a collection warning only when it prevents a requested collection.

## Permissions and namespaces

Lens runs as the current user. Process command lines, descriptors, journals, sockets, certificates
and logs can therefore be partial. Linux capabilities, macOS privacy controls, containers and remote
terminal products can each change what the same binary sees. The hostname, resolver, mounts, users,
devices and process list always describe the namespace in which Lens is running.

## Terminal support

Lens responds to terminal resizing and progressively removes secondary columns on compact displays.
Colour defaults to automatic background detection; use `--theme light`, `--theme dark` or the
`LENS_THEME` environment variable when a browser terminal, serial link or multiplexer does not expose
reliable colour metadata. `lens-top --ascii` is available where Unicode drawing characters are not
rendered correctly.

Minimum tier-1 operating-system releases, the stable 1.x surface and deprecation rules are defined
in [`SUPPORT_POLICY.md`](SUPPORT_POLICY.md).
