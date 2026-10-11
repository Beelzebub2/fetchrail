#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux ]] || { echo 'Run this script on Linux.' >&2; exit 1; }
source /etc/os-release
identity=" ${ID:-} ${ID_LIKE:-} "
if [[ $identity =~ (ubuntu|debian|linuxmint|kali) ]]; then
  install=(apt-get install -y --no-install-recommends build-essential cmake ninja-build pkg-config curl wget file git zip unzip tar perl nasm python3 libwebkit2gtk-4.1-dev libgtk-3-dev libglib2.0-bin libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf xdg-utils ca-certificates xvfb dbus-x11 rpm desktop-file-utils fonts-dejavu-core)
elif [[ $identity =~ (fedora|rhel|centos) ]]; then
  install=(dnf install -y gcc gcc-c++ make cmake ninja-build pkgconf-pkg-config curl wget file git zip unzip tar perl nasm webkit2gtk4.1-devel gtk3-devel libxdo-devel openssl-devel libayatana-appindicator-gtk3-devel librsvg2-devel patchelf xdg-utils ca-certificates xorg-x11-server-Xvfb dbus-x11 rpm-build desktop-file-utils)
elif [[ $identity =~ (arch|manjaro) ]]; then
  install=(pacman -S --needed --noconfirm base-devel cmake ninja pkgconf curl wget file git zip unzip tar perl nasm webkit2gtk-4.1 gtk3 xdotool openssl libappindicator librsvg patchelf xdg-utils ca-certificates xorg-server-xvfb dbus desktop-file-utils)
elif [[ $identity =~ (opensuse|suse) ]]; then
  install=(zypper --non-interactive install gcc gcc-c++ make cmake ninja pkg-config curl wget file git zip unzip tar perl nasm webkit2gtk3-devel gtk3-devel libxdo-devel libopenssl-devel libappindicator3-devel librsvg-devel patchelf xdg-utils ca-certificates xvfb-run dbus-1-x11 rpm-build desktop-file-utils)
else
  echo "No automatic dependency recipe for ${PRETTY_NAME:-this distro}. Install GTK3, WebKitGTK 4.1 development files, xdo, OpenSSL, an AppIndicator library, librsvg, C/C++ build tools, CMake, Ninja, pkg-config, Git, curl, zip, unzip, tar, Perl, NASM and patchelf. See docs/linux.md." >&2
  exit 1
fi
if [[ ${1:-} == --print ]]; then printf '%q ' "${install[@]}"; printf '\n'; exit; fi
if [[ $EUID == 0 ]]; then elevate=(); else elevate=(sudo); fi
if [[ ${install[0]} == apt-get ]]; then "${elevate[@]}" apt-get update; fi
# Arch must already be fully updated; never perform a partial system upgrade.
"${elevate[@]}" "${install[@]}"
