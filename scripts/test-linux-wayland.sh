#!/usr/bin/env bash
set -euo pipefail
command -v weston >/dev/null || { echo 'Install Weston to test a native Wayland session.' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
runtime=$(mktemp -d "${TMPDIR:-/tmp}/fetchrail-wayland.XXXXXX")
chmod 700 "$runtime"
compositor=''
cleanup() {
  local result=$?
  trap - EXIT
  if [[ -n $compositor ]]; then kill "$compositor" 2>/dev/null || true; wait "$compositor" 2>/dev/null || true; fi
  # Portal services may release their private FUSE mounts after D-Bus exits.
  # Unmount only this fixture's paths and never traverse another filesystem.
  local helper mount mounted
  helper=$(command -v fusermount3 || command -v fusermount || true)
  for _ in {1..20}; do
    mounted=false
    for mount in "$runtime/doc" "$runtime/gvfs"; do
      if [[ -n $helper ]] && mountpoint -q "$mount"; then
        timeout 2 "$helper" -u -- "$mount" 2>/dev/null || true
      fi
      if mountpoint -q "$mount"; then mounted=true; fi
    done
    if [[ $mounted == false ]] && rm -rf --one-file-system -- "$runtime" 2>/dev/null; then exit "$result"; fi
    sleep 0.25
  done
  echo "Unable to remove private Wayland runtime: $runtime" >&2
  exit 1
}
trap cleanup EXIT
cd "$repo"
mkdir -p artifacts/linux-qualification
export XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=fetchrail-test GDK_BACKEND=wayland
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=Weston-headless
unset DISPLAY
renderer=--use-pixman
help=$(weston --help 2>&1 || true)
[[ $help != *--renderer* ]] || renderer=--renderer=pixman
weston --backend=headless-backend.so "$renderer" --socket="$WAYLAND_DISPLAY" \
  --idle-time=0 --width=1280 --height=1024 --log=artifacts/linux-qualification/weston.log &
compositor=$!
for _ in {1..100}; do
  [[ ! -S "$runtime/$WAYLAND_DISPLAY" ]] || break
  kill -0 "$compositor" 2>/dev/null || { cat artifacts/linux-qualification/weston.log; exit 1; }
  sleep 0.1
done
[[ -S "$runtime/$WAYLAND_DISPLAY" ]] || { echo 'Wayland compositor did not start.' >&2; exit 1; }
if ! dbus-run-session -- node scripts/test-linux-desktop.mjs "$@" 2>artifacts/linux-qualification/wayland-desktop.log; then
  tail -n 60 artifacts/linux-qualification/wayland-desktop.log >&2
  exit 1
fi
