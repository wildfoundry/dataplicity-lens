# Structured output contract

## Versioning

Every JSON document and JSON Lines record contains `schema_version`. Version `2` may gain additive,
optional fields. Removing a field, changing its meaning or changing a unit requires a new schema
version.

## Document shape

```json
{
  "schema_version": "2",
  "generated_at": "2026-08-03T00:00:00Z",
  "host": {},
  "processes": [],
  "services": [],
  "log_sources": [],
  "logs": [],
  "mounts": [],
  "filesystems": [],
  "deleted_open_files": [],
  "block_devices": [],
  "interfaces": [],
  "routes": [],
  "sockets": [],
  "cellular_modems": [],
  "clock": {},
  "dns": {},
  "certificates": [],
  "accounts": [],
  "groups": [],
  "hardware": {},
  "temperatures": [],
  "hardware_devices": [],
  "findings": [],
  "relationships": [],
  "build": {},
  "collection_warnings": []
}
```

`generated_at` is UTC RFC 3339. Memory and I/O totals are bytes. I/O rates are bytes per second.
Durations are seconds. CPU and memory are percentages. Process CPU can exceed 100 on a multicore host.

## Field reference

| Field | Contents and units |
| --- | --- |
| `schema_version` | String identifying the compatibility contract. Current value: `"2"`. |
| `generated_at` | UTC RFC 3339 collection timestamp. |
| `host` | Hostname, kernel/OS, uptime seconds, logical CPU count, CPU percentage, load averages, memory and process counts. |
| `processes` | Identity, parent, command, user, state, CPU/memory percentages, byte counters/rates, runtime seconds and optional service/container context. |
| `services` | Native service name, load/active/sub state, description and optional restart count. |
| `log_sources` | Stable source identifier and source kind. |
| `logs` | Timestamp, source, optional unit/priority, message and adjacent-repeat count. |
| `mounts` | Source, target, filesystem, total/used/available bytes, percentage and optional inode totals. |
| `filesystems` | Filesystem identifiers and kinds used by relationships. |
| `deleted_open_files` | Optional PID, command, deleted path and optional retained byte count. |
| `block_devices` | Device name/type, size bytes, optional filesystem and mount points. |
| `interfaces` | Name, state, addresses and optional cumulative RX/TX byte counters. |
| `routes` | Stable ID, destination, optional gateway/interface and the native raw record. |
| `sockets` | Protocol/state, local/peer endpoints and optional owner/PID. |
| `cellular_modems` | Modem state, radio technology, signal percentage, operator and optional SIM metadata. |
| `clock` | Optional timezone, NTP synchronization state and reporting service. |
| `dns` | Resolver source, nameservers and search domains visible in the current namespace. |
| `certificates` | Path plus optional public subject, issuer and expiry. Private keys are not read. |
| `accounts` / `groups` | Local account and group metadata visible through the operating-system databases. |
| `hardware` | Machine/board/firmware identity and optional Raspberry Pi firmware status. |
| `temperatures` | Sensor name/source, degrees Celsius and optional driver maximum/critical thresholds. |
| `hardware_devices` | USB/serial kind, path and optional manufacturer, IDs and serial number. |
| `findings` | Severity, title, summary, evidence, related entities and suggested next checks. |
| `relationships` | Typed links between entity identifiers. |
| `build` | Optional version, commit, target and builder identity. |
| `collection_warnings` | Human-readable reasons requested data was partial or unavailable. |

IDs and counters are integers unless the model explicitly uses a percentage or rate. Optional values
can be absent or `null`; empty arrays mean that collection completed with no matching records only
when there is no corresponding collection warning.

`hardware` contains machine, board, firmware and Raspberry Pi firmware state. `temperatures` records
degrees Celsius with optional driver-provided maximum and critical limits. `hardware_devices` contains
USB and serial inventory with stable paths and identifiers where the operating system exposes them.
Each interface can include cumulative `rx_bytes` and `tx_bytes` counters. The interactive network
view derives current rates and history charts from successive samples; one-shot JSON keeps the source
counters so consumers can calculate deltas over their own sampling interval.
`certificates` contains locally managed public certificates visible to the current user. Subject,
issuer and expiry are populated when OpenSSL can inspect the file; Lens does not open private keys or
enumerate the distribution's complete root CA catalogue.

Cellular ICCIDs, hardware serial numbers, account names, internal addresses, certificate paths and
log messages can be sensitive inventory. JSON does not redact fields that the current user explicitly
requested. Review a snapshot before attaching it to a public issue.

## Nullable and unavailable data

Fields that cannot be inferred or read are `null`. Permission-denied details are also named in the
process `unavailable_fields` array. A disappearing `/proc` entry is omitted because the process no
longer existed at the point a coherent record could be collected.

## JSON Lines

JSON Lines output emits one host record, then process records, then finding records. Each line has:

```json
{"schema_version":"2","generated_at":"...","record_type":"process","value":{}}
```

Terminal colour, spacing and glyphs never appear in structured output.

Specialist commands emit the same document shape but collect only their relevant domain, leaving
unrelated collections empty. This keeps scripts on one schema without making a log query wait for
storage, network or service commands. Optional owner, restart and inode fields are omitted or `null`
when the host does not expose them.

## Collection completeness and exit status

Collection warnings do not change the process exit status. A command that produced a partial snapshot
exits successfully and names the missing source in `collection_warnings`; a command that could not
produce its requested result exits with status `1`. Argument parsing errors exit with status `2`.
See [`CLI_CONTRACT.md`](CLI_CONTRACT.md) for the complete stdout, stderr and action contract.
