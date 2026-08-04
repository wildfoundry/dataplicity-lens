# Linux cellular modems and SIMs

On Linux, `lens-net` discovers mobile-broadband hardware managed by ModemManager. ModemManager is the
normal abstraction over QMI, MBIM and AT-based 3G/4G/5G devices; Lens does not open modem control
ports or send vendor commands.

```sh
lens-net
lens-net --plain
lens-net --json | jq '.cellular_modems'
```

The current model includes modem manufacturer/model, state, access technology, signal percentage,
registered operator and the active SIM path, ICCID and operator where ModemManager exposes them.
Collection is optional: if `mmcli` is not installed, ordinary interfaces, routes and listeners work
without a warning. If ModemManager is installed but fails or times out, the reason appears with the
other unavailable network data.

NetworkManager remains responsible for APN and connection profiles. Lens does not create, overwrite,
connect or disconnect a profile in the current beta.

The 1.0 qualification work still requires richer bearer/IP and SIM-lock state, default masking with an
explicit reveal path, failure-specific Health findings, hotplug tests and real QMI plus MBIM/5G
hardware results on Raspberry Pi OS. Those are tracked in DAT-234 and are not implied merely by the
presence of `cellular_modems` in a beta snapshot.
