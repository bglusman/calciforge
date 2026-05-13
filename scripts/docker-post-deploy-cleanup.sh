#!/usr/bin/env bash
set -euo pipefail

min_free_gb="${CALCIFORGE_DOCKER_MIN_FREE_GB:-10}"
docker_root="${CALCIFORGE_DOCKER_ROOT:-/var/lib/docker}"

free_gb() {
    df -BG --output=avail "$docker_root" | awk 'NR == 2 { gsub(/G/, "", $1); print $1 }'
}

before="$(free_gb)"
printf 'Docker filesystem free before cleanup: %sG\n' "$before"

docker builder prune -af
docker image prune -f

after="$(free_gb)"
printf 'Docker filesystem free after cleanup: %sG\n' "$after"

if [ "$after" -lt "$min_free_gb" ]; then
    printf 'warning: Docker filesystem free space %sG is below target %sG\n' "$after" "$min_free_gb" >&2
fi
