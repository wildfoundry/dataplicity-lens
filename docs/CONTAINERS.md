# Docker and Podman containers

`lens-containers` inventories containers from Docker and Podman when those tools are installed
and live for the current user.

```sh
lens-containers
lens-containers --plain
lens-containers --json | jq '.containers'
lens-containers --runtime docker --state running --fail-if-empty --quiet
```

## Presence and permissions

Collection follows an optional-runtime contract:

- If `docker` or `podman` is not on `PATH`, that runtime contributes nothing and no warning is added.
- If the CLI is present but the daemon is not live (connection refused / daemon not running), that
  runtime is skipped silently.
- If the CLI is present and the manager is live, but the current user cannot use the socket or is
  missing the usual `docker` / `podman` group membership, Lens emits a `collection_warnings` entry
  naming the runtime and the access problem.
- Both runtimes may contribute rows in one snapshot. Rows are never merged across runtimes when
  names collide.

Lens invokes the CLI as the current user, so `DOCKER_HOST` and rootless Podman environments are
honoured by the runtime itself.

## Row fields

Each container exposes:

| Field | Meaning |
| --- | --- |
| `runtime` | `docker` or `podman` |
| `id` | Full container ID |
| `name` | Primary name |
| `image` | Image reference |
| `status` | Human status string from the runtime |
| `state` | Normalized state (`running`, `exited`, `created`, `paused`, …) |
| `created` | Created timestamp string from the runtime when available |
| `ports` | Published ports display string |

## Filters and asserts

Use `--runtime`, `--name`, `--image`, `--status`, `--state`, `--filter` and `--match` to narrow the
list. Suite scripting flags (`--fail-if-empty`, `--expect-count*`, `--fields`, `--jsonl`, `--quiet`)
apply to the filtered container set.

## Actions

Supported actions are `start`, `stop` and `restart`. They follow the same safety model as service
actions: `--dry-run` plans, `--yes` confirms, selectors must resolve to exactly one container, and
`--expect-status` with `--wait` can verify the resulting state.

```sh
lens-containers --action restart --name edge-mqtt --match exact --dry-run
lens-containers --action restart --target a1b2c3d4e5f6 --runtime docker --yes
lens-containers --action start --name metrics-agent --match exact --expect-status running --yes
```

Images, volumes, networks, logs and stats are out of scope for this specialist. Process cgroup
inference in `lens-top` remains the complementary process-level container hint.
