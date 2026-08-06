# Linux cellular modems and SIMs

On Linux, `lens-net` discovers mobile-broadband hardware managed by ModemManager, the normal
abstraction over QMI, MBIM and AT-based 3G/4G/5G devices.

```sh
lens-net
lens-net --plain
lens-net --json | jq '.cellular_modems'
```

The model includes modem manufacturer/model, state, access technology, signal percentage,
registered operator and the active SIM path, ICCID and operator where ModemManager exposes them.
Collection is optional: if `mmcli` is not installed, ordinary interfaces, routes and listeners work
without a warning. If ModemManager is installed but fails or times out, the reason appears with the
other unavailable network data.
