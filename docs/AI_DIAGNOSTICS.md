# AI and accelerator diagnostics

`lens --focus ai` is a read-only diagnostic handoff for AI Fleet. It reads the Dataplicity agent's
authoritative state document and does not run vendor utilities, discover models independently, send
network requests, or keep a second cache. The default source is:

```text
/run/dataplicity/agent/ai-state.json
```

An integration or test harness may provide a different absolute path with `--agent-state PATH`.
Relative paths are rejected. Input is capped at 2 MiB and parsed as data; no value is interpolated
into a command.

## Fleet handoff

AI Fleet can open Lens with one or more exact context selectors:

```sh
lens --focus ai --model vision-v4
lens --focus ai --runtime inference-main
lens --focus ai --accelerator pci-0000:01:00.0
lens --focus ai --source camera/front
lens --focus ai --model vision-v4 --runtime inference-main --source camera/front
```

Selectors are optional, case-insensitive exact matches and may be combined. They only narrow the
rendered copy; they never change agent state. `--focus ai` produces a focused one-shot view so it is
safe to launch from another terminal UI. Use `--json --fields ai` for structured handoff.

## Agent state document

The agent publishes the schema-v2 `ai` object directly:

```json
{
  "source": "dataplicity-agent",
  "observed_at": "2026-09-17T00:30:00Z",
  "accelerators": [{
    "stable_id": "pci-0000:01:00.0",
    "kind": "nvidia-gpu",
    "driver_version": "550.54",
    "runtime_version": "CUDA 12.4",
    "memory_total_bytes": 8589934592,
    "memory_used_bytes": 6442450944,
    "temperature_c": 71.0,
    "utilisation_percent": 88.0,
    "throttling": false,
    "unavailable_fields": []
  }],
  "model_store": {
    "desired": "vision-v4",
    "staged": "vision-v4",
    "current": "vision-v3",
    "previous": "vision-v2",
    "digest": "sha256:5b39d7d1",
    "cache_used_bytes": 14000000000,
    "cache_limit_bytes": 16000000000,
    "unavailable_fields": []
  },
  "runtimes": [{
    "id": "inference-main",
    "process_id": 8192,
    "active_model": "vision-v4",
    "loaded_model": "vision-v3",
    "input_source": "camera/front",
    "input_age_seconds": 42,
    "queue_depth": 7,
    "fallback": "cpu",
    "errors": [],
    "unavailable_fields": []
  }],
  "unavailable": []
}
```

Accelerator `stable_id` values are supplied by the agent and must survive device-index changes.
Unavailable fields use a field name, one of `missing_tool`, `absent`, `permission_denied`, `stale`,
`not_reported` or `malformed`, and optional detail. Lens adds `stale` when `observed_at` is over five
minutes old. Missing files, denied access and malformed JSON are shown explicitly instead of being
misrepresented as an empty healthy state.

The runtime view calls out active-versus-loaded model divergence, input freshness, queue depth,
fallback and current errors. It does not infer runtime state from the host process list.
