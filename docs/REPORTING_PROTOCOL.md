# Lens report protocol v1

Transport-independent UTF-8 JSON observations. This is independent of the embedded Lens snapshot
schema (`2`). Breaking envelope meaning, units or framing requires a new protocol version. Additive
fields and future capabilities are tolerated and must remain data, never executable directives.

## Local handoff

One nonblocking writer and one reader, JSON Lines through a protected Linux FIFO. One compact
JSON document per newline; JSON strings escape embedded newlines/control characters. Maximum frame
payload: 262144 bytes, excluding the newline. Writes are chunked and may be partial; the reader
keeps a bounded incomplete-frame buffer, drops oversize/invalid frames, resets at EOF and resyncs
on newline. Writers prefix reconnection with a newline and cut expired incomplete frames.
The pipe is not the outage buffer. There is no acknowledgement or request channel back into Lens.

```json
{
  "protocol_version": 1,
  "schema_version": "2",
  "session_id": "00000000-0000-4000-8000-000000000001",
  "boot_id": "kernel-boot-id",
  "sequence": 1,
  "collected_at_ms": 1000000,
  "expires_at_ms": 2800000,
  "capabilities": ["processes", "hardware"],
  "coverage": {"processes": "complete", "hardware": "partial"},
  "snapshot": {"schema_version": "2", "host": {}, "processes": []}
}
```

The abbreviated snapshot above illustrates nesting; production snapshots use the shared Lens model.
Times are UTC Unix milliseconds, sequence is a positive unsigned integer, session is a new UUID per
collector process. Expiry is collection time plus exactly 1800000 ms. Sequence gaps, a new session,
partial coverage and source-level unavailable fields must be visible rather than interpreted as
confirmed exits/recovery. Current v1 conservatively marks domain coverage partial if collection
warnings or truncation occurred. Process keys are (registered host, boot_id, PID, start_time_ticks).
Container keys are (registered host, runtime, runtime ID); neither display names nor PIDs are keys.

The snapshot's `host.refresh_interval_ms` describes the short CPU/I/O measurement window, not the
reporting cadence. Standard snapshot units still apply: bytes, bytes/sec, degrees Celsius, seconds,
percentages (a process may exceed 100% across cores). Network/disk source counters are cumulative;
consumers may compute deltas across reports only when identity and counter continuity hold.

## Registration and upload

`POST /device-gateway/lens/provision/` reuses existing agent organisation-code validation/rotation,
provisioning throttles, Device records, class assignment and credentials. JSON fields:
`provisioning_key`, `name`, stable v4 `registration_id`, optional `expected_device_hash` for repair.
The existing response includes `hash_id`, `device_secret` and the echoed `registration_id`.
Registration retries with that UUID and org produce the same identity, serialized under the org
lock; no new enrolment database or rotation mechanism is introduced. The publisher never retries
registration automatically and never sends an org code as a process argument or stores it.

`POST /device-gateway/lens/{device_hash}/reports/` uses the issued `Authorization: ApiKey ...`.
The URL host is bound to the authenticated device; organisation ownership comes from that record,
not payload fields. Request shape: an object with `protocol_version: 1` and `reports`, an array of one to four
report objects in the envelope format above, with the newest report first.

Maximum encoded request: 1048576 bytes. No compression is required. Response 201 acknowledges the
bounded batch (including already expired entries discarded by the receiver); repeated report IDs
are idempotent and do not extend expiry. Expired entries are not persisted. The receiver provides
`Retry-After` on 429/503. The publisher has exactly one retry owner and ignores response bodies
except during explicit registration; it accepts no commands, URLs, sampling changes or actions.

`GET /api/developer/devices/{device_hash}/lens/?limit=1` reads the latest report using existing
organisation/role/device-access checks. `limit` is capped at 32 for the rolling window. Responses
are private/no-store. An empty report list is missing current observations, not proof of health.
This endpoint supplies the data for Console; it does not add a second presence/history model.

## Receiver safety

A dedicated Redis store must have persistence/snapshots/backups disabled, a memory ceiling and
`noeviction` policy; exhausted/unavailable storage fails closed with 503/Retry-After. Never fall
back to SQL, object storage, durable telemetry or shared persistent caches. Reports have individual
absolute expiries; duplicate/replayed uploads do not refresh them. Per-host indexes cap at 32 and
remove the oldest payload keys. Every read rechecks expiry; indexes contain references, not reports.

Atomic admission gates default to one request/device/minute, 2000 requests and 8 MiB/org/minute,
and 10000 requests and 256 MiB/global/minute. These are explicit deployment budgets, not guarantees
that every fleet size/payload fits: size cache capacity and budgets together, monitor rejection and
coverage, and keep hard limits. Shared Redis scripts enforce the gates across workers.
