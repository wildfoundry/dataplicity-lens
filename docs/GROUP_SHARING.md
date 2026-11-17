# Shared group summaries

In the Dataplicity Lens web workspace, tag servers and create a group under **Groups & sharing**. A server must match every selected tag. Enable **Allow data sharing in this group** explicitly; it starts disabled. Current and future matching servers can then read one another’s reported summaries, within the same organisation only. Identical tags in another organisation grant no access.

The viewer reads Dataplicity’s received CPU, memory, storage and reporting summaries. It never runs a collector, contacts peers, fetches logs or process command lines, or executes commands. Even the requesting server is represented by its last server-received report.

Run using the existing publisher account so the private registration file stays private:

```sh
sudo -u lens-publisher lens-group
sudo -u lens-publisher lens-group --group GROUP_UUID --watch
sudo -u lens-publisher lens-group --group GROUP_UUID --page 2 --json
```

Replace `GROUP_UUID` with the ID displayed in Groups & sharing. The first command opens an interactive group chooser in a terminal. In the Linux Lens cockpit, **g** opens the same chooser using the publisher account. Use `--once` for a readable single-page report or `--json` for structured output. Do not copy registration credentials into shell arguments or world-readable files. `--offset` follows the exact `next` offset from a JSON response, including reduced pages. `--state-dir` supports a different private publisher directory.

Each request retrieves at most 20 summaries and 32 KiB. Pages are demand-driven; there is no eager whole-fleet download, push subscription or missed-update replay. Background refresh waits at least 60 seconds plus jitter. User-requested navigation and search can read after a minimum 12-second gap following a successful request, within the existing six-page device budget; neither can shorten a failure or Retry-After hold. The viewer allows one in-flight request, caps connection time at 3 seconds and the whole request at 10 seconds, refuses redirects and caps headers and bodies.

The dashboard shows a selectable server table with CPU, memory and persistent storage percentages and bars, reporting status and readable report age. The selected-server panel includes the reported hardware service tag, CPU model and threads, installed RAM, process count, failed-service count, running-container count, and original report time in UTC. Unavailable readings show an em dash. Summary counts and sorting are explicitly scoped to the current page.

**↑/↓** selects a row; **Enter** opens a group or full-screen details; **PgUp/PgDn** scrolls details; **g** returns to the chooser; **/** searches server names across the group; **s** sorts the current page; **n/p** changes page; **?** opens help; **q** exits. `--search NAME` works with plain/JSON output too. `--demo` previews labelled sample data without credentials or a network connection. Network reads run on one background worker, keeping quit responsive during a stalled connection. Failed, reset, partial, oversized or invalid responses never replace the last complete view. Repeated transport failures halve the requested page size down to one summary so a thin link can recover without repeatedly transferring a full page. That view is marked stale; authentication or membership failures clear it and stop polling. Each displayed row includes the original report timestamp, which can precede the latest successful read.

Retry-After (including HTTP dates) is respected with jittered exponential backoff. Switching pages preserves failure cooldowns. Restarting the client cannot bypass server request/byte admission. The server reserves the entire page budget before loading telemetry: at most six full reservations per device per minute, with a shared 4 MiB/minute organisation cap. Disconnected attempts retain their reservation. If the telemetry store is unavailable the request returns 503 with a retry delay, never another data source.

A merge makes this available for the next package build; installing a released package is separate from merging source.
