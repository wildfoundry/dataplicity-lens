# Repository settings

This file records the GitHub settings expected for `wildfoundry/dataplicity-lens`. Visibility and
access still require administrator action in GitHub when they change.

## Repository

- Visibility: **public** (Apache-2.0; docs at `https://lens.dataplicity.com/`)
- Issues enabled; Discussions, Projects and wiki disabled
- Squash merge only; delete head branches on merge; auto-merge and update-branch enabled
- Default workflow token permission: **read**
- Actions: selected (GitHub-owned + verified creators + explicit allowlist for pinned third-party actions)
- Dependabot alerts/security updates, secret scanning and push protection enabled
- Private vulnerability reporting enabled
- GitHub Pages: workflow-built from `site/`, custom domain `lens.dataplicity.com`

## Release automation tokens

Cross-repo GitHub credentials used by Actions must belong to `dataplicity-release-bot`. See
`.github/RELEASE_TOKENS.md`. Workflows refuse personal-account PATs.

## Access (no public write)

- Organisation default repository permission: **none**
- Maintain access granted only to internal staff team `@wildfoundry/dataplicity-web-developers`
- No outside collaborators
- External contributions are accepted only via pull requests from forks

## Main branch

`.github/ruleset-main.json` is the source for the `Protect main` ruleset. It requires:

- A pull request
- At least one approving review, including CODEOWNERS
- Last-push approval and resolved conversations
- The complete Linux/macOS CI matrix
- Linear history; no force pushes; no branch deletion

Apply or refresh with:

```sh
gh api --method PUT repos/wildfoundry/dataplicity-lens/rulesets/<id> \
  --input .github/ruleset-main.json
```

## Version tags

`.github/ruleset-tags.json` protects `v*` tags from deletion and force-updates.

Version tags are accepted by the release workflow only when their version matches the workspace and
the tagged commit is reachable from `main`; see `docs/RELEASING.md`.
