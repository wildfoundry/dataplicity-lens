# Operational actions

Lens actions are deliberately narrow. Inspect first, use `--dry-run`, then add `--yes` when the
target and intended change are correct.

## Processes

In `lens-top`, select a process and press `a`. Choose a signal, review the pinned name, PID and
start-time identity, then press `y` to confirm. The result remains visible until it is dismissed.
The equivalent non-interactive commands are:

```sh
lens-top --signal term --pid 4242 --dry-run
lens-top --signal term --pid 4242 --yes
lens-top --signal hup --pid 4242 --yes --json
lens-top --signal term --exact-name nginx --expect-name nginx --dry-run
```

Supported signals are `term`, `kill`, `hup`, `int`, `stop` and `cont`. Lens refuses PID 0, PID 1 and
its own PID. Target a process with `--pid` and/or filters (`--exact-name`, `--filter-name`,
`--filter-user`, `--filter-service`, …). The selector must resolve to exactly one process; zero or
multiple matches exit `2` before mutation. Lens records PID/start-time identity, optionally checks
`--expect-start-ticks` and `--expect-name`, resolves again just before execution, and stops if the
identity changed. A successful signal delivery does not imply the process exited; the result states
whether the same process remains and its observed state.

## systemd services

In the interactive `lens-services` view on Linux, select a unit and press `a`. Choose an action,
review the exact unit name and press `y` to confirm. The same operations are available to scripts:

```sh
lens-services --action restart --target nginx.service --dry-run
lens-services --action restart --target nginx.service --yes
lens-services --action restart --name nginx.service --match exact --expect-active active --yes
lens-services --action stop --target my-worker.service --yes --json
```

Supported actions are `start`, `stop`, `restart`, `enable` and `disable`. Provide `--target` as one
exact unit name, or a selector (`--name`, `--service`, `--filter`, `--active`) that resolves to
exactly one unit. Ambiguous or empty selectors exit `2`. After execution, `--expect-active STATE`
polls until the unit reaches that active state or `--wait` elapses (default `2s`), then exits `3` on
miss. Lens invokes `systemctl` as the current user, applies a 15-second deadline and reads service
state again after the command. Existing system policy decides whether the user is authorised; Lens
does not embed credentials or elevate its whole process.

launchd actions are not currently enabled. The command fails before changing state on macOS.

## Diagnostic shell

Press `!` in an interactive view to open a local command panel. On a wide terminal it occupies a
right-hand card so the live system view remains visible; on a compact terminal it uses nearly the
whole screen. Commands run asynchronously through the invoking user's normal shell. Lens does not
save command history, supply credentials or elevate the command. Press `Esc` to close the panel.

The shell is for deliberate, one-shot diagnostics. It is visually separate from typed Lens actions:
Lens cannot preflight or verify an arbitrary shell command in the way it verifies a process signal or
service operation.

## Automation behavior

- `--dry-run` does not require `--yes` and never changes state.
- A real action without `--yes` exits non-zero before execution.
- `--json` returns one action outcome object on stdout; `--quiet` suppresses stdout.
- Invalid targets, ambiguous selectors, stale processes, permission denial, command failure and
  verification failure exit non-zero with an explanation on stderr (`2` for usage/ambiguity, `1` for
  execution failure, `3` for unmet `--expect-active`).
- Lens does not retry a state-changing action automatically.
