#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux ]] || { echo 'Run the integration fixtures on Linux.' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
for tool in python3 gio desktop-file-validate; do command -v "$tool" >/dev/null || { echo "Missing fixture tool: $tool" >&2; exit 1; }; done
fixture=$(mktemp -d /tmp/fetchrail-integration.XXXXXX)
[[ $fixture == /tmp/fetchrail-integration.* ]]
trap 'rm -rf -- "$fixture"' EXIT
export XDG_DATA_HOME="$fixture/data with space'quote%"
export FETCHRAIL_INTEGRATION_TEST_LOG="$fixture/launch.log"
image="$fixture/test image.AppImage"
cat > "$image" <<'IMAGE'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == --appimage-extract ]]; then
  mkdir -p squashfs-root
  cat > squashfs-root/AppRun <<'APP'
#!/usr/bin/env bash
printf '%s\n' "$@" >> "$FETCHRAIL_INTEGRATION_TEST_LOG"
APP
  chmod 700 squashfs-root/AppRun
else
  printf '%s\n' "$@" >> "$FETCHRAIL_INTEGRATION_TEST_LOG"
fi
IMAGE
chmod 700 "$image"
managed="$XDG_DATA_HOME/com.rrmtools.braid/portable"
desktop="$XDG_DATA_HOME/applications/com.rrmtools.braid.desktop"
bash "$repo/scripts/install-appimage.sh" "$image"
cmp "$image" "$managed/Fetchrail.AppImage"
[[ $(stat -c %a "$managed/Fetchrail.AppImage") == 700 ]]
desktop-file-validate "$desktop"
uri='https://example.invalid/a%20b?x=one&y=two'
gio launch "$desktop" "$uri"
for _ in {1..100}; do grep -Fxq -- "$uri" "$FETCHRAIL_INTEGRATION_TEST_LOG" 2>/dev/null && break; sleep 0.05; done
grep -Fxq -- "$uri" "$FETCHRAIL_INTEGRATION_TEST_LOG"
# Repairing the same managed copy must preserve its bytes and remain launchable.
bash "$repo/scripts/install-appimage.sh" "$managed/Fetchrail.AppImage"
cmp "$image" "$managed/Fetchrail.AppImage"
ln -s "$image" "$fixture/input-link.AppImage"
if bash "$repo/scripts/install-appimage.sh" "$fixture/input-link.AppImage"; then echo 'A symlink source was accepted.' >&2; exit 1; fi
bash "$repo/scripts/install-appimage.sh" "$image" --extract
[[ -x "$managed/AppDir/AppRun" ]]
[[ -x "$managed/AppDir/fetchrail-launcher" ]]
desktop-file-validate "$desktop"
gio launch "$desktop" "$uri"
if bash "$repo/scripts/install-appimage.sh" "$image" --extract; then echo 'An existing AppDir was overwritten.' >&2; exit 1; fi
state="$XDG_DATA_HOME/com.rrmtools.braid/downloads.json"
printf '%s\n' 'fixture resume state' > "$state"
bash "$repo/scripts/remove-linux-integration.sh" "$managed/AppDir/AppRun"
[[ ! -e $desktop && -x $managed/AppDir/AppRun && -f $state ]]
grep -Fxq -- '--remove-integration' "$FETCHRAIL_INTEGRATION_TEST_LOG"
grep -Fxq -- 'fixture resume state' "$state"
# A foreign entry at the same path must survive removal.
printf '%s\n' '[Desktop Entry]' 'Name=Another application' > "$desktop"
bash "$repo/scripts/remove-linux-integration.sh" "$managed/AppDir/AppRun"
grep -Fxq -- 'Name=Another application' "$desktop"
echo 'PASS: GLib desktop launch with quoted paths, AppImage repair/extraction, symlink rejection, owned-entry removal and preserved resume state.'
