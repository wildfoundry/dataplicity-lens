# Roadmap

## Foundation

- Stabilise the process snapshot schema.
- Add fixture-driven Linux collector tests.
- Establish accessibility and terminal compatibility baselines.
- Publish signed packages and provenance attestations.

## Tool sequence

1. `lens-services`
2. `lens-logs`
3. `lens-disk`
4. `lens-net`
5. `lens-health`

The order is intentional: services and logs create the strongest shared
operational context, while `lens-health` should compose mature findings from all
other domains rather than invent a second diagnostics system.
