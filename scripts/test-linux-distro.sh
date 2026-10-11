#!/usr/bin/env bash
# Run installed-package checks in an official distro userspace, without Docker.
# The kernel is supplied by the Linux host; this does not certify distro kernels.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Run this isolated userspace fixture through sudo.' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
distro=${1:?Choose fedora44, mint223, kali or arch}
inputs=$(realpath "${2:?Provide the directory containing packages, torrent-harness, tauri-driver and node}")
prepared=${3:-}
case "$distro" in
  fedora44)
    url=https://download.fedoraproject.org/pub/fedora/linux/releases/44/Container/x86_64/images/Fedora-WSL-Base-44-1.7.x86_64.wsl
    digest=2e5b153ba4b639952bf546be577fc19b832fe8944caa7de342b32f10da7d319a ;;
  arch)
    url=https://fastly.mirror.pkgbuild.com/wsl/2026.10.01.179549/archlinux-2026.10.01.179549.wsl
    digest=d2ef183f58cd65bfce52d5a122fe4936f7f2ac0af6b1b427681afd28d1f64b17 ;;
  kali)
    url=https://kali.download/wsl-images/kali-2026.2/kali-linux-2026.2-wsl-rootfs-amd64.wsl
    digest=1b172389e9109e9bb0c3d1fa18eda078271484dbcef8dbee4aab8b1f369466c6 ;;
  mint223)
    url=https://pub.linuxmint.io/stable/22.3/linuxmint-22.3-xfce-64bit.iso
    digest=45a835b5dddaf40e84d776549e0b19b3fbd49673b6cc6434ebddbfcd217df776 ;;
  *) echo "Unknown distro: $distro" >&2; exit 1 ;;
esac
for tool in node tauri-driver torrent-harness; do [[ -f $inputs/$tool ]] || { echo "Missing fixture input: $tool" >&2; exit 1; }; done
[[ -d $inputs/browser-extension/dist ]] || { echo 'Missing built browser extension fixtures.' >&2; exit 1; }
work=$(mktemp -d /tmp/fetchrail-distro.XXXXXX)
output=$repo/artifacts/linux-qualification/$distro
mkdir -p "$output"
rm -f -- "$output/result.json"
mounts=()
cleanup() {
  local result=$?
  trap - EXIT
  # D-Bus activated services can outlive the test session. Stop only processes
  # rooted in this newly created fixture before removing its files or mounts.
  if [[ -z $prepared && -d $root && $root == "$work/root" ]]; then
    local process pid
    local owned=()
    for process in /proc/[0-9]*/root; do
      [[ $(readlink "$process" 2>/dev/null || true) == "$root" ]] || continue
      pid=${process#/proc/}; pid=${pid%/root}
      owned+=("$pid")
      kill -TERM "$pid" 2>/dev/null || true
    done
    if (( ${#owned[@]} )); then
      sleep 1
      for pid in "${owned[@]}"; do
        [[ $(readlink "/proc/$pid/root" 2>/dev/null || true) == "$root" ]] || continue
        kill -KILL "$pid" 2>/dev/null || true
      done
    fi
  fi
  if [[ -d ${project:-}/artifacts/linux-qualification ]]; then cp -a "$project/artifacts/linux-qualification/." "$output/" || result=1; fi
  for ((i=${#mounts[@]}-1; i>=0; i--)); do
    if ! umount -R "${mounts[i]}"; then echo "Fixture mount still busy: ${mounts[i]}" >&2; result=1; fi
  done
  # Never recursively remove a tree that still contains a fixture bind mount.
  if [[ -n $prepared ]] || { ! mountpoint -q "$root/proc" && ! mountpoint -q "$root/dev" && ! mountpoint -q "$root/sys"; }; then rm -rf -- "$work"; fi
  if (( result != 0 )); then printf '{"status":"failed","manualAcceptance":"pending"}\n' > "$output/result.json"; fi
  chown -R "${SUDO_UID:-0}:${SUDO_GID:-0}" "$output" || result=1
  exit "$result"
}
root=$work/root
trap cleanup EXIT
if [[ -n $prepared ]]; then
  # Reuse an already prepared developer fixture, retaining its files and mounts.
  root=$(realpath "$prepared")
  [[ -f $root/etc/os-release && $root != / ]] || exit 1
else
  mkdir -p "$root"
  curl --fail --location --retry 3 --output "$work/image" "$url"
  printf '%s  %s\n' "$digest" "$work/image" | sha256sum --check
  if [[ $distro == mint223 ]]; then
    xorriso -osirrox on -indev "$work/image" -extract /casper/filesystem.squashfs "$work/root.squashfs"
    rm -- "$work/image"
    unsquashfs -processors 2 -d "$root" "$work/root.squashfs"
    rm -- "$work/root.squashfs"
  else
    tar --numeric-owner -xaf "$work/image" -C "$root"
    rm -- "$work/image"
  fi
fi
for directory in proc dev sys; do
  mkdir -p "$root/$directory"
  if ! mountpoint -q "$root/$directory"; then
    mount --rbind "/$directory" "$root/$directory"
    mount --make-rslave "$root/$directory"
    mounts+=("$root/$directory")
  fi
done
if [[ -z $prepared ]]; then
  rm -f -- "$root/etc/resolv.conf"
  cp /etc/resolv.conf "$root/etc/resolv.conf"
  case "$distro" in
    fedora44)
      chroot "$root" dnf -y --setopt=install_weak_deps=False install webkit2gtk4.1 webkitgtk6.0 libayatana-appindicator-gtk3 libxdo xdg-utils ca-certificates xorg-x11-server-Xvfb xorg-x11-xauth dbus-daemon dbus-x11 dejavu-sans-fonts binutils shadow-utils gawk weston ;;
    arch)
      chroot "$root" pacman-key --init
      chroot "$root" pacman-key --populate archlinux
      chroot "$root" pacman -Syu --needed --noconfirm webkit2gtk-4.1 webkitgtk-6.0 gtk3 xdotool libappindicator xdg-utils ca-certificates xorg-server-xvfb xorg-xauth dbus ttf-dejavu binutils fakeroot gcc make gawk weston ;;
    kali|mint223)
      printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"
      chmod 755 "$root/usr/sbin/policy-rc.d"
      driver=webkit2gtk-driver
      [[ $distro != kali ]] || driver=webkitgtk-webdriver
      chroot "$root" apt-get update
      chroot "$root" env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends libwebkit2gtk-4.1-0 libxdo3 libayatana-appindicator3-1 libglib2.0-bin xdg-utils ca-certificates xvfb xauth dbus-x11 fonts-dejavu-core binutils "$driver" weston ;;
  esac
fi
chroot "$root" id fetchrail >/dev/null 2>&1 || chroot "$root" useradd -m -u 1000 fetchrail
install -d -m700 -o1000 -g1000 "$root/run/user/1000"
install -Dm755 "$inputs/node" "$root/usr/local/bin/node"
install -Dm755 "$inputs/tauri-driver" "$root/usr/local/bin/tauri-driver"
install -Dm755 /usr/bin/xvfb-run "$root/usr/local/bin/xvfb-run"
project=$root/home/fetchrail/distro-check
mkdir -p "$project/src-tauri/target/release" "$project/package"
cp -a "$repo/scripts" "$project/"
cp -a "$inputs/browser-extension" "$project/"
cp "$repo/package.json" "$project/"
install -m755 "$inputs/torrent-harness" "$project/src-tauri/target/release/torrent-harness"
case "$distro" in
  fedora44)
    packages=("$inputs"/*.rpm); [[ ${#packages[@]} == 1 ]]
    cp "${packages[0]}" "$root/tmp/fetchrail.rpm"
    operation=install
    if chroot "$root" rpm -q fetchrail >/dev/null 2>&1; then operation=reinstall; fi
    chroot "$root" dnf -y "$operation" /tmp/fetchrail.rpm ;;
  kali|mint223) packages=("$inputs"/*.deb); [[ ${#packages[@]} == 1 ]]; cp "${packages[0]}" "$root/tmp/fetchrail.deb"; chroot "$root" env DEBIAN_FRONTEND=noninteractive apt-get install --reinstall -y /tmp/fetchrail.deb ;;
  arch)
    archives=("$inputs"/*.tar.gz); [[ ${#archives[@]} == 1 ]]
    version=$("$inputs/node" -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1])).version)' "$repo/package.json")
    cp "${archives[0]}" "$project/package/Fetchrail-v$version-linux-x64.tar.gz"
    archive_digest=$(sha256sum "${archives[0]}" | cut -d' ' -f1)
    # Only this fixture's x64 input is built; no ARM artifact is published here.
    sed -e "s/@VERSION@/$version/g" -e "s/@X64_SHA256@/$archive_digest/g" -e "s/@ARM64_SHA256@/$(printf '0%.0s' {1..64})/g" \
      -e 's#${url}/releases/download/v${pkgver}/Fetchrail-v${pkgver}-linux-x64.tar.gz#file:///home/fetchrail/distro-check/package/Fetchrail-v${pkgver}-linux-x64.tar.gz#' \
      "$repo/src-tauri/linux/PKGBUILD.in" > "$project/package/PKGBUILD"
    chown -R 1000:1000 "$project"
    chroot --userspec=1000:1000 "$root" env HOME=/home/fetchrail PATH=/usr/local/bin:/usr/bin:/bin bash -c 'cd /home/fetchrail/distro-check/package; makepkg --noconfirm --cleanbuild --force'
    chroot "$root" pacman -U --noconfirm "/home/fetchrail/distro-check/package/fetchrail-bin-$version-1-x86_64.pkg.tar.zst" ;;
esac
chown -R 1000:1000 "$project"
printf '{"distro":"%s","image":"%s","imageSha256":"%s","kernelScope":"host kernel; distro userspace only"}\n' "$distro" "$url" "$digest" > "$output/image.json"
chroot --userspec=1000:1000 "$root" env HOME=/home/fetchrail XDG_RUNTIME_DIR=/run/user/1000 \
  PATH=/usr/local/bin:/usr/bin:/bin GDK_BACKEND=x11 XDG_SESSION_TYPE=x11 XDG_CURRENT_DESKTOP=Xvfb \
  FETCHRAIL_TEST_DISPLAY="${FETCHRAIL_TEST_DISPLAY:-99}" \
  FETCHRAIL_SOURCE_REVISION="${FETCHRAIL_SOURCE_REVISION:-unknown}" \
  FETCHRAIL_SOURCE_ARCHIVE_SHA256="${FETCHRAIL_SOURCE_ARCHIVE_SHA256:-}" \
  FETCHRAIL_QUALIFICATION_ENVIRONMENT="$distro official userspace in native chroot; host kernel; physical desktop and machine actions pending" \
  FETCHRAIL_TORRENT_HARNESS=/home/fetchrail/distro-check/src-tauri/target/release/torrent-harness \
  bash -c 'set -e; cd /home/fetchrail/distro-check; xvfb-run -n "$FETCHRAIL_TEST_DISPLAY" -a dbus-run-session -- node scripts/qualify-linux.mjs /usr/bin/fetchrail; xvfb-run -n "$FETCHRAIL_TEST_DISPLAY" -a dbus-run-session -- node scripts/test-linux-desktop.mjs /usr/bin/fetchrail; bash scripts/test-linux-wayland.sh /usr/bin/fetchrail'
case "$distro" in
  fedora44) chroot "$root" dnf -y remove fetchrail ;;
  arch) chroot "$root" pacman -R --noconfirm fetchrail-bin ;;
  kali|mint223) chroot "$root" env DEBIAN_FRONTEND=noninteractive apt-get remove -y fetchrail ;;
esac
[[ ! -e $root/usr/bin/fetchrail ]]
printf '{"status":"passed","checks":["package install","HTTP/native bridge/restart","torrent swarms","extension fixtures","native X11 controls","native Wayland controls","package removal"],"manualAcceptance":"pending"}\n' > "$output/result.json"
