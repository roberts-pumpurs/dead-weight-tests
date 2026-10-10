#!/usr/bin/env bash
# Applies .github/rulesets/*.json to this repository, matching rulesets by name.
# Run it with an admin gh login after changing a ruleset file:
#   .github/rulesets/apply.sh
set -euo pipefail

repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
for file in "$(dirname "$0")"/*.json; do
  name="$(jq -r .name "$file")"
  id="$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name == \"$name\") | .id")"
  if [[ -n "$id" ]]; then
    gh api -X PUT "repos/$repo/rulesets/$id" --input "$file" >/dev/null
    echo "updated ruleset $name ($id)"
  else
    gh api -X POST "repos/$repo/rulesets" --input "$file" >/dev/null
    echo "created ruleset $name"
  fi
done
