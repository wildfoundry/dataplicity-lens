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
  "findings": [],
  "relationships": [],
  "build": {},
  "collection_warnings": []
}
```

`generated_at` is UTC RFC 3339. Memory and I/O totals are bytes. I/O rates are bytes per second.
Durations are seconds. CPU and memory are percentages. Process CPU can exceed 100 on a multicore host.

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
