# Command-line and automation contract

All Lens commands use the same rules for interactive use, one-shot output, diagnostics and errors.
This document describes the current `0.3.x` contract. Command-specific filters and actions are listed
by each command's `--help` output and on the documentation site.

## Choosing an output mode

| Invocation | Behaviour |
| --- | --- |
| `lens` or a specialist on a terminal | Open the interactive view. |
| `--once` | Collect once, print human-readable text and exit. |
| `--plain` | Explicitly select the same one-shot human-readable format. |
| `--json` | Print one schema-versioned JSON document and exit. |
| `lens-top --jsonl` | Print schema-versioned JSON Lines and exit. |
| Output redirected to a pipe or file | Default to one-shot plain text unless a structured format was selected. |

`--once` and `--plain` are intentionally equivalent in output. Use `--once` when the important
instruction is “do not open the TUI”; use `--plain` when the important instruction is “produce text,
not JSON”. A broken downstream pipe is treated as a normal exit so commands such as
`lens-top --plain | head` do not report a failure.

## Limits and filters

The specialist default is 1,000 rows per result type. `--limit 0` removes that cap and can produce a
large response on a busy host. A specialist collects only its own domain before applying its filter;
it does not wait for unrelated services, logs, disks or network probes.

`--filter` is a case-insensitive match over the values rendered by that specialist. Domain-specific
options are rejected when used with the wrong command rather than ignored. For example,
`--severity` belongs to `lens-logs`, while `--service` belongs to `lens-services` and `lens-logs`.

`lens-top` samples twice to calculate process CPU and I/O rates. Its implicit one-shot sampling window
is capped at 250 ms for shell use. An explicit `--interval`, such as `--interval 2s`, is honoured as
the one-shot measurement window as well as the interactive refresh interval.

## Standard streams

- Normal plain, JSON, JSON Lines and action results are written to standard output.
- Usage errors, collection failures, rejected actions and terminal failures are written to standard
  error with the command name.
- Collection warnings are part of a successful snapshot. Plain output labels them as unavailable
  data; JSON places them in `collection_warnings`.
- Interactive commands use the terminal's alternate screen and restore it on normal exit, error,
  `Ctrl+C` and panic handling covered by the terminal guard.

## Exit status

| Status | Meaning |
| --- | --- |
| `0` | The command ran and produced its result. Findings and collection warnings can still be present. |
| `1` | Collection, configuration, output or a requested action failed. The error is on standard error. |
| `2` | Command-line syntax or argument validation failed before collection. |

A Health finding does not by itself change the exit status. Scheduled checks should inspect both
`findings` and `collection_warnings`:

```sh
lens-health --json | jq -e '
  (.collection_warnings | length) == 0 and
  ([.findings[] | select(.severity == "critical")] | length) == 0
'
```

This keeps transport and collection failure separate from the policy an operator chooses for a
warning or critical finding.

## Actions

The non-interactive action contract is deliberately explicit:

- `lens-top --signal SIGNAL --pid PID` targets one visible process.
- `lens-services --action ACTION --target UNIT` targets one exact systemd unit on Linux.
- `--dry-run` prints the planned target and action without changing state.
- Execution requires `--yes`; absence of confirmation is an error.
- Process identity is checked again immediately before a signal is sent.
- A completed action prints a structured or human-readable result and the observed post-action state.

Actions use the invoking user's existing operating-system authority. Lens does not retain credentials
or elevate the complete application.

## Stability

Command names, common flags and schema version `2` are the compatibility surface for `0.3.x`.
Additional optional JSON fields may be added without changing the schema version. Removing a field,
changing its meaning or changing a unit requires a new schema version and release note.
