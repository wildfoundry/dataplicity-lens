# Fleet reporting preparation

Status: design requirements, not an implemented daemon, command, service unit or wire protocol.
This change only extends local Hardware identity and its existing outputs.

## Explicit activation and privileges

Installing or upgrading Lens must leave collection/reporting daemons disabled and stopped. Do not
auto-enable services, register hosts, connect to Dataplicity, or grant additional permissions in
package hooks. An administrator must take a separate explicit step to enable reporting.

The collector runs as a dedicated non-root user with no added capabilities by default. Preserve
normal local Lens use. Restricted observations remain unavailable with explicit reasons; do not
automatically elevate, invoke sudo, grant broad device access, or join privileged groups such as
docker, lxd or disk merely to improve coverage. Any additional access must be a deliberate,
narrow local policy choice. There is no general-purpose privileged helper in this design.

The publisher is a separate, more restricted non-root service identity. It reads the reporting pipe
and holds only its own Dataplicity registration/transport credentials and bounded upload buffer.
It has no need to read host procfs, sysfs, journals, serials, container sockets or block devices.
It cannot signal the collector, change its configuration, or write to its input. Do not run it as
root or share the collector's UID, supplementary groups or hardware access.

## One-way local handoff

The collector publishes framed, versioned observations through a one-way Linux pipe/FIFO; the
publisher receives only the read end. Restrict pipe permissions and parent-directory ownership
so the publisher cannot become a writer or replace the pipe. Payload data is not spooled to disk.
Nonblocking publishing must never stall collection when no reader exists or the pipe is full.
Handle missing readers, partial writes, frame boundaries and restart recovery explicitly.
The kernel pipe capacity is not the history or outage buffer.

The collector retains a bounded in-memory observation window. The publisher separately retains
a bounded in-memory upload backlog during network outages. Both enforce age and byte caps,
discard oldest complete records when necessary, and report coverage gaps. There is no indefinite
retry queue and no reverse command channel. A crashed process loses its memory buffer.

Opening a Console view navigates already reported observations. It must not request shell access,
process signals, service/container actions, additional privileges or collection commands on a host.

## Cadence, identity and retention

- Default reporting cadence is approximately one minute, separate from interactive Lens refresh.
  Cache stable identity and avoid repeating expensive inventory probes at display refresh rates.
- Processes and containers are ephemeral. Process identity includes host, boot, PID and start
  time; container identity includes host, runtime and runtime ID. Reused names/PIDs are new
  entities. Keep current, absent-from-complete-sample, unknown and stale states distinct.
- Process history is sampled, not a promise to capture every short-lived process or exact exit time.
- All observations, including hardware identity and recent logs, expire 30 minutes after collection
  throughout the local and central reporting path. Upload/replay does not reset observation age.
  Byte limits can shorten coverage and must be visible. Prioritise current complete state after
  reconnection; discard expired backlog and mark gaps instead of presenting old data as live.
- Only server registration/name and the existing Dataplicity connectivity/history records persist.
  Reuse existing connectivity semantics and retention; do not invent a Lens-specific history system.
  Server records require explicit deletion. Hardware serials are transient observations, not new
  permanent inventory or authentication keys.

## Registration and protocol requirements

Use a simple organisation enrolment code and the existing agent code rotation conventions. Pairing
issues individual host credentials. Do not build a second account, connectivity or rotation system.
Registration/credential handling belongs to the publisher, not the hardware collector.

Before implementing transport, establish a documented versioned protocol with fixtures and
compatibility tests. Reports need host/session/boot identity as applicable, sequence number,
collection time, expiry, domain/capability identity, collection completeness and coverage limits.
Define framing/size limits, units, snapshot boundaries and resynchronisation after loss. A partial
sample or dropped frame must not imply entity disappearance. Periodic self-contained inventory
allows recovery without requests back into Lens. Capabilities allow future read-only Lens domains
to appear centrally. Additive fields must be tolerated; breaking meanings/units require a new
version. Carry the shared Lens model and schema version explicitly; terminal text is not the wire
contract. The current schema-version-2 hardware additions prepare that model but do not establish
a complete fleet wire protocol.
