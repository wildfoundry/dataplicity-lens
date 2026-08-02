# Architecture

Lens is a Rust workspace with deliberately narrow dependency directions.

```text
                   +------------------+
                   |    lens-top      |
                   +---------+--------+
                             |
               +-------------+-------------+
               |             |             |
          lens-ui       lens-output   platform-linux
               |             |             |
               +-------+-----+-------------+
                       |
                   lens-core
                       |
          +------------+------------+
          |                         |
     lens-model             lens-diagnostics
          |                         |
          +-------------------------+
```

`lens-model` owns serialisable facts. `lens-core` owns source contracts and
view transformations. Platform crates collect facts. UI and output crates
render them. Applications compose these pieces and contain almost no business
logic.

## Dependency rules

1. Models cannot depend on a platform or renderer.
2. Platform crates cannot depend on UI crates.
3. Output formats consume the same model used by the TUI.
4. Diagnostics return data, not preformatted terminal output.
5. Applications may choose policy, but reusable behaviour belongs in crates.
6. Future tools should add domain types to `lens-model` only when those types
   are genuinely shared.

## Future applications

The workspace layout is intended to accommodate additional binaries under
`apps/` without cloning infrastructure:

- `lens-services`: service state, dependencies, restart history
- `lens-logs`: structured local log exploration and correlation
- `lens-disk`: filesystem, inode, mount, and I/O pressure
- `lens-net`: sockets, routes, listeners, DNS, and traffic
- `lens-health`: concise cross-domain host assessment

Common keyboard handling, formatting, severity language, output conventions,
and Linux capability detection should remain shared.
