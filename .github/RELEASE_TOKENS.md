# Release automation tokens

CI must not use personal GitHub credentials. Cross-repo release automation uses PATs owned by the
limited machine user **`dataplicity-release-bot`**.

| Secret | Used by | Required access |
| --- | --- | --- |
| `HOMEBREW_TAP_TOKEN` | `Update Homebrew tap` | Contents + Pull requests write on `wildfoundry/homebrew-tap` only |

`GITHUB_TOKEN` remains the default for same-repo Actions (releases, Pages, dispatch, checkout of
this repository).

## Enforcement

Workflows call `scripts/verify-release-bot-token.sh`, which fails if the secret is missing or owned
by any account other than `dataplicity-release-bot`.

Run **Actions → Audit release tokens** to re-check owners after rotating secrets.

## Rotating a token

1. Sign in as **`dataplicity-release-bot`** (not a staff admin account).
2. Create a fine-grained PAT scoped to `wildfoundry/homebrew-tap` with Contents + Pull requests write.
3. Set the repository secret with `gh secret set HOMEBREW_TAP_TOKEN --repo wildfoundry/dataplicity-lens`.
4. Re-run **Audit release tokens**.
5. Revoke the previous PAT.
