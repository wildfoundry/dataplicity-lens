# Repository settings

This file records the GitHub settings expected for `wildfoundry/dataplicity-lens`. It does not change
repository visibility. Visibility remains an explicit WildFoundry decision.

## Repository

- Description: `A fast terminal toolkit for understanding Linux and macOS systems.`
- Homepage: `https://lens.dataplicity.com/`
- Issues enabled; Discussions and wiki disabled
- Squash merge only, automatic head-branch deletion, auto-merge and branch updates enabled
- Default workflow token permission: read-only
- Dependabot alerts, security updates and automated fixes enabled

## Main branch

`.github/ruleset-main.json` is the source for the `Protect main` repository ruleset. It requires pull
requests, conversation resolution and the complete Linux/macOS CI matrix, and blocks force pushes and
deletions. It can be applied by a repository administrator with:

```sh
gh api repos/wildfoundry/dataplicity-lens/rulesets \
  --method POST \
  --input .github/ruleset-main.json
```

Version tags are accepted by the release workflow only when their version matches the workspace and
the tagged commit is reachable from `main`; see `docs/RELEASING.md`.
