#!/usr/bin/env bash
# Usage: it.sh [area] [libtest args...]; IT_PROFILES=a,b adds optional compose profiles.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

if ! docker info >/dev/null 2>&1; then
  if command -v orb >/dev/null 2>&1; then orb start; else echo "Docker is not running" >&2; exit 1; fi
fi

free_kib=$(df -k "$HOME" | awk 'NR==2 {print $4}')
profiles=()
kept=()
for profile in ${IT_PROFILES//,/ }; do
  if [[ "$profile" == nextcloud ]] && (( free_kib < 20 * 1024 * 1024 )); then
    echo "Less than 20 GiB free: leaving the nextcloud profile off" >&2
    continue
  fi
  profiles+=(--profile "$profile")
  kept+=("$profile")
done
IT_PROFILES=$(IFS=,; echo "${kept[*]-}")
export IT_PROFILES

pull=(--pull missing)
if (( free_kib < 5 * 1024 * 1024 )); then
  echo "Less than 5 GiB free: not pulling images, using only those already here" >&2
  pull=(--pull never)
fi

mkdir -p "$here/.state"
touch "$here/.state/dovecot-locked-users"
mkdir -p "$here/.state/images" "$here/.state/pdf" "$here/.state/sl"
docker compose -f "$here/compose.yaml" ${profiles[@]+"${profiles[@]}"} up -d --wait "${pull[@]}"

if [[ ",$IT_PROFILES," == *",docker,"* ]]; then
  if (( free_kib < 5 * 1024 * 1024 )); then
    echo "Less than 5 GiB free: not building images for the docker profile" >&2
  else
    docker build -t holtmail:dev "$root"
    if ! docker image inspect holtmail:it-old >/dev/null 2>&1; then
      old="$(mktemp -d)/holtmail-fed635b"
      git -C "$root" worktree add --detach "$old" fed635b
      status=0
      docker build -t holtmail:it-old "$old" || status=$?
      git -C "$root" worktree remove --force "$old"
      (( status == 0 )) || exit "$status"
    fi
  fi
fi

cd "$root"
cargo build -p holtmail-server --bin holtmail
export HOLTMAIL_BIN="${CARGO_TARGET_DIR:-$root/target}/debug/holtmail"
export HOLTMAIL_IT=1

target=()
if [[ $# -gt 0 && -f "$here/tests/$1.rs" ]]; then
  target=(--test "$1")
  shift
fi
cargo test -p holtmail-integration ${target[@]+"${target[@]}"} -- --test-threads="${IT_THREADS:-4}" "$@"
