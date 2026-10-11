#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux ]] || { echo 'Run on Linux.' >&2; exit 1; }
image=${1:?Usage: install-appimage.sh /path/Fetchrail.AppImage [--extract]}
command -v python3 >/dev/null || { echo 'Install python3 to generate the desktop entry safely.' >&2; exit 1; }
[[ ${2:-} == '' || ${2:-} == --extract ]] || { echo 'Unknown installation option.' >&2; exit 1; }
[[ ! -L "$image" ]] || { echo 'Select the actual AppImage file, not a symlink.' >&2; exit 1; }
image=$(realpath -- "$image")
[[ -f "$image" && ! -L "$image" ]] || { echo 'Expected a regular AppImage file.' >&2; exit 1; }
data=${XDG_DATA_HOME:-"$HOME/.local/share"}
[[ $data == /* ]] || data="$HOME/.local/share"
managed="$data/com.rrmtools.braid/portable"
mkdir -p -- "$managed" "$data/applications" "$data/icons/hicolor/128x128/apps"
chmod 700 -- "$managed"
if [[ ${2:-} == --extract ]]; then
  # Extraction is persistent: browser hosts and autostart must survive process exit.
  staging=$(mktemp -d "$managed/extract.XXXXXX")
  trap 'rm -rf -- "$staging"' EXIT
  chmod +x -- "$image"
  (cd -- "$staging" && "$image" --appimage-extract >/dev/null)
  [[ -x "$staging/squashfs-root/AppRun" ]] || { echo 'AppImage extraction failed.' >&2; exit 1; }
  [[ ! -e "$managed/AppDir" ]] || { echo 'An extracted installation exists. Remove its integration and move AppDir aside before installing again.' >&2; exit 1; }
  mv -- "$staging/squashfs-root" "$managed/AppDir"
  # Keep AppRun's library environment when browser hosts or autostart relaunch it.
  target="$managed/AppDir/fetchrail-launcher"
  cat > "$target" <<'LAUNCHER'
#!/bin/sh
unset APPIMAGE APPDIR OWD
appdir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export FETCHRAIL_LAUNCHER="$appdir/fetchrail-launcher"
exec "$appdir/AppRun" "$@"
LAUNCHER
  chmod 700 -- "$target"
else
  target="$managed/Fetchrail.AppImage"
  [[ "$image" != "$target" ]] || { echo 'The AppImage is already installed here; repairing integration.'; }
  if [[ "$image" != "$target" ]]; then
  temporary=$(mktemp "$managed/Fetchrail.XXXXXX")
  trap 'rm -f -- "$temporary"' EXIT
  cp -- "$image" "$temporary"
  chmod 700 -- "$temporary"
  mv -f -- "$temporary" "$target"
  fi
fi
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
icon="$script_dir/../src-tauri/icons/128x128.png"
[[ -f "$icon" ]] || icon="$script_dir/fetchrail.png"
if [[ ! -f "$icon" && -d "$managed/AppDir" ]]; then
  icon=$(find "$managed/AppDir" -type f -name 'fetchrail.png' -print -quit)
fi
if [[ -f "$icon" ]]; then cp -- "$icon" "$data/icons/hicolor/128x128/apps/fetchrail.png"; fi
TARGET="$target" DATA="$data" python3 - <<'PY'
import os, pathlib
target=os.environ['TARGET']
if any(c in target for c in '\n\r\0'): raise SystemExit('Unsupported launcher path')
escaped=target.replace('\\', '\\\\\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%','%%')
path=pathlib.Path(os.environ['DATA'])/'applications/com.rrmtools.braid.desktop'
temporary=path.with_suffix('.desktop.tmp')
temporary.write_text('[Desktop Entry]\nType=Application\nName=Fetchrail\nExec=/usr/bin/env "'+escaped+'" %U\nIcon=fetchrail\nTerminal=false\nCategories=Network;FileTransfer;\nMimeType=x-scheme-handler/magnet;application/x-bittorrent;\n',encoding='utf-8')
temporary.replace(path)
PY
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Installed at $target. Native browser hosts are registered on first launch."
"$target" --background </dev/null >/dev/null 2>&1 &
