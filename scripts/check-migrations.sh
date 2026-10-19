#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
base="${1:-origin/main}"
head="${2:-HEAD}"

version() { sed -n 's|^migrations/\([0-9]*\)_.*\.sql$|\1|p' <<<"$1"; }

base_files="$(git ls-tree --name-only "$base" migrations/ | grep '\.sql$' || true)"
head_files="$(git ls-tree --name-only "$head" migrations/ | grep '\.sql$' || true)"
latest="$(sort <<<"$base_files" | tail -n 1)"

status=0
while IFS= read -r file; do
  [[ -n "$file" ]] || continue
  v="$(version "$file")"
  name="${file#migrations/"$v"_}"
  clash="$(printf '%s\n%s\n' "$base_files" "$head_files" | sort -u | grep "^migrations/${v}_" | grep -vx "$file" || true)"
  if [[ -n "$clash" ]]; then
    echo "$file: version $v is already used by ${clash//$'\n'/, }." >&2
  elif ((10#$v <= 10#$(version "$latest"))); then
    echo "$file: version $v sorts before $latest, the latest migration on $base." >&2
  else
    continue
  fi
  echo "  fix: rename to \$(scripts/next-migration-version)_$name" >&2
  status=1
done < <(git diff --name-only --no-renames --diff-filter=A "$(git merge-base "$base" "$head")" "$head" -- 'migrations/*.sql')

((status == 0)) && echo "Migration versions are unique and after $latest."
exit "$status"
