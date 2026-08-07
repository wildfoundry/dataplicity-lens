#!/usr/bin/env bash
# Verify a GitHub token is owned by the limited release-bot account.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <token-env-var-name> <expected-login>" >&2
  exit 2
fi

token_var="$1"
expected_login="$2"
token="${!token_var-}"

if [[ -z "$token" ]]; then
  echo "Missing required token in environment variable: ${token_var}" >&2
  exit 1
fi

login="$(
  curl -fsS \
    -H "Authorization: Bearer ${token}" \
    -H "Accept: application/vnd.github+json" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    https://api.github.com/user \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("login",""))'
)"

if [[ -z "$login" ]]; then
  echo "${token_var}: could not resolve GitHub login for token" >&2
  exit 1
fi

echo "${token_var} owner: ${login}"

if [[ "$login" != "$expected_login" ]]; then
  echo "${token_var} must be a PAT owned by '${expected_login}', not '${login}'." >&2
  echo "Mint a fine-grained token as ${expected_login} with least privilege, then update the repository secret." >&2
  exit 1
fi
