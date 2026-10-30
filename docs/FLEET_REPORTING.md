# Optional Linux fleet reporting

`lens-daemon` collects read-only Lens observations. `lens-publisher` independently reads a one-way
FIFO and uploads those observations to Dataplicity. Neither service is enabled or started by
installation, upgrades, package hooks, or registration. Local Lens continues to work independently.
There is no remote shell, action, incoming command, collector configuration, or resampling API.

## Enable explicitly

Deploy the matching Lens gateway API and its dedicated volatile cache before enabling clients.
On a Linux system with systemd, install the Lens package, then run:

```sh
sudo lens-reporting-enable --name server-01
```

The setup helper prompts for the existing Dataplicity organisation code without echoing it or
putting it in the process arguments. It explicitly creates the service accounts and FIFO, registers
as the non-root publisher, and only after successful registration enables/starts both services.
Organisation code ownership and rotation reuse agent provisioning. Code rotation does not replace
already issued host credentials. Registration retries reuse the saved registration UUID, so a lost
response cannot create another server record. No code is stored in the credentials file.

For an alternative Dataplicity deployment, supply `--server-url https://your-api.example` at initial
setup. Only HTTPS origins are accepted. Credentials never follow redirects to another endpoint.
Existing credentials and their origin are retained; a setup rerun does not silently move a host.

To explicitly repair credentials or a halted publisher with the org code:

```sh
sudo lens-reporting-enable --name server-01 --refresh
```

Existing cooldowns still apply; repeatedly invoking setup cannot defeat a `Retry-After` hold.
Manual setup can be retried after the reported cooldown. Credential files contain the host's
registration identity and must not be copied into a fleet image.

To stop reporting while retaining registration:

```sh
sudo systemctl disable --now lens-daemon.service lens-publisher.service
```

Registration, naming, deletion, presence and connectivity history use Dataplicity's existing device
records and policies. No parallel connectivity history or new offline threshold is introduced.
Backoff may therefore result in an offline classification under the existing presence rules.

## Privilege boundary

- Collector: `lens-collector`, no added capabilities or privileged groups. It receives no org code
  or transport credentials. Its service permits local Unix/netlink inspection and denies IP traffic.
- Publisher: `lens-publisher`, with only the `lens-report` read group. It receives only the FIFO's
  read side; the FIFO is collector-owned, mode `0240`, in a `0710` directory the publisher cannot
  change. Its sandbox hides host hardware/process inventory and logs, and provides no collector
  socket, signal authority, command channel or write access to the reporting directory.
- Root-restricted observations remain explicitly unavailable. Do not join docker/lxd/disk groups
  or elevate either daemon to make the display look more complete.
- The administrative setup helper runs as root solely to create accounts/resources and enable
  services. Both executable daemons refuse to run as root. Foreground processes outside systemd
  do not inherit the reference unit's filesystem/network isolation.

## Resource and retention limits

The collector starts with random delay, collects no more frequently than once per minute, and runs
one inventory worker with a 20-second wall-time deadline. Timeout/oversize workers are terminated
as a process group; a failed sample waits for the next interval instead of retrying immediately.
CPU/process I/O rates use two cheap process scans about 250 ms apart, followed by one domain
inventory pass; these are short-window samples, not minute averages. Cumulative counters retain
their existing Lens meanings. Brief processes/containers between samples can be missed.

Each report is at most 256 KiB. Arrays/strings are bounded; truncation is labelled partial coverage.
Process identity includes boot and kernel start ticks, not PID alone. Containers keep runtime IDs.
Unknown/inaccessible observations do not prove absence. All future snapshot fields remain data.

The collector and publisher each retain at most 4 MiB / 32 reports in memory for at most 30 minutes.
A slow/missing reader cannot block collection. Oldest complete records are discarded when full.
Interrupted FIFO frames are discarded/resynchronised. Report/session sequence gaps expose missing
coverage. Process restart loses its buffer. Service memory/CPU/task limits add an outer safety bound;
swap and core dumps are disabled in the reference units (requires the corresponding cgroup support).

The publisher always sends current state first, with at most three retained older reports in the
same request at default cadence. Under a longer server interval it sends only the newest report. It never sends a rapid backlog sweep: at most one request is in flight, with at least
the server-directed interval (default 60 seconds) plus jitter after success. It uploads no expired report. Server expiry uses original
collection time, never arrival/retry time. Clocks must be reasonably synchronised; reports more
than 60 seconds in the future are rejected.

## Storm protection

- Random initial phases and ongoing jitter avoid synchronized fleet startup/recovery.
- Before each network request, persist a jittered uncertainty hold of at least five minutes or the current server interval, whichever is longer. A crash during an
  ambiguous request cannot turn service restarts into immediate requests. A clean success replaces
  that hold with the normal server-directed jittered cadence.
- 429, 408, all 5xx and transport errors use exponential randomized backoff, with a locally capped
  exponential component (up to one hour) and the persisted uncertainty floor.
- `Retry-After` accepts seconds or HTTP dates, including server clock-offset protection. Its delay
  is a floor, with additional jitter. It is NEVER shortened to the local exponential cap. Extremely
  large valid holds saturate to a quiet hold rather than overflowing or causing a busy retry loop.
- New observations, an empty/overwritten queue, success on an unrelated path or a service restart
  do not clear an upload failure hold. No retry middleware, concurrent uploads, redirect following,
  alternate endpoint probing or automatic re-enrolment is used.
- 401/403/404/405/410/415/426 and redirects halt transmission until explicit operator repair.
  Other non-retryable 4xx discard the rejected bounded batch and retain normal pacing, avoiding
  poison-record loops. TLS is verified; request time is capped at 20 seconds, connection time at 5.
- A singleton lock prevents duplicate publishers using the same state directory. Startup limits
  and five-minute restart delays also apply to local setup/runtime failures.

Only credentials, registration identity and a small scheduling gate are written to
`/var/lib/lens-publisher/state.json` (owner-only, atomic and synced). No observations are written
there. Failure to read/persist valid scheduling state stops network publishing rather than falling
back to an aggressive schedule. `status` exposes registration/scheduling metadata, never secrets.

See [the wire protocol](REPORTING_PROTOCOL.md) and [design constraints](FLEET_REPORTING_DESIGN.md).

## Server-directed upload cadence

The server can announce `Lens-Report-Interval-Seconds` from 60 to 14400 seconds. Global and org
policy resolve on the gateway; an org can slow reporting further but cannot shorten the global
minimum. The publisher remembers the last valid setting across restarts and missing/invalid
headers, and `status` shows it. Decreases take effect after the current hold, when the next response
is received. No extra policy polling or control path into the collector is created.

An interval change reduces upload frequency and, above one minute, sends just the newest observation.
Collector cadence remains independent. Buffers/expiry remain at 30 minutes, so intervals above
30 minutes leave periods with no current central observations. Existing connectivity classification
is unchanged. Retry-After/backoff can require a delay beyond the four-hour normal-policy maximum.
