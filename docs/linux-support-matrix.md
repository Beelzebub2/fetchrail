# Linux qualification evidence

Updated 10 October 2026. **No distro has full certification yet.** Ubuntu 22.04.5 passed the complete x64 native build, Rust/HTTP/torrent fixtures, X11/Wayland WebKitGTK controls and package checks through WSL2. Fedora 44, Mint 22.3, Kali 2026.2 and current Arch official userspaces also passed installed-app HTTP/torrent/extension fixtures, native X11/Wayland controls and package removal in separate non-root chroots on that kernel. Full distro kernels/policies/desktops and ARM64 execution remain pending. See [local validation and performance evidence](linux-validation.md).

| Target | Artifacts / build path | Automated evidence | Desktop acceptance | Status |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04 x86_64 | Native baseline; unsigned deb/rpm/AppImage/archive built | Version 0.5.3, WSL2: 57 Rust tests; HTTP/native bridge; TCP/uTP swarms; deb install/remove; AppImage/extracted/archive launch passed | Xvfb/X11, WSLg/Wayland and headless Weston controls passed; GNOME/KDE/GPU/power acceptance pending | Automated checks passed; experimental |
| Ubuntu 24.04 aarch64 | Native ARM CI; deb/rpm/AppImage/archive | Pending CI | Pending ARM hardware | Experimental |
| Ubuntu 24.04/26.04 x86_64 | deb/AppImage | Pending | Pending | Experimental |
| Fedora 44 x86_64 | Fedora rpm/AppImage | DNF install/remove and installed HTTP/native bridge, torrent/extension fixtures passed | Non-root X11 and headless Wayland passed; GNOME/KDE/SELinux enforcing pending | Userspace checks passed; experimental |
| Mint 22.3 x86_64 | deb/AppImage | Official Xfce ISO userspace, APT install/remove and installed HTTP/torrent/extension fixtures passed | Non-root X11 and headless Wayland passed; full Cinnamon/MATE/Xfce/file managers pending | Userspace checks passed; experimental |
| Kali 2026.2 last snapshot x86_64 | deb/AppImage | Official WSL userspace, APT install/remove and installed HTTP/torrent/extension fixtures passed | Non-root X11 and headless Wayland passed; full Xfce/Firefox ESR pending | Experimental; unresolved restart allocator diagnostic |
| Arch image 2026.10.01, repositories 10 Oct 2026 x86_64 | Verified binary PKGBUILD/AppImage | Source checksum, makepkg, pacman install/remove and installed HTTP/torrent/extension fixtures passed | Non-root X11 and headless Wayland passed; full KDE/GNOME/Xfce pending | Userspace checks passed; experimental |
| Fedora 43, Mint 21.3/LMDE 7, Kali rolling | Family packages/AppImage | Pending independent version acceptance | Pending | Experimental |
| Debian / Ubuntu and Arch derivatives | Native family packages/AppImage | Pending independent base checks | Pending | Experimental |
| openSUSE / immutable glibc desktops | Source/portable; SUSE development recipe | Pending | Pending | Experimental |
| NixOS | Binary derivation with stable wrapper | Pending derivation evaluation/build | Pending | Experimental |
| Gentoo / Void / other glibc systems | Matching source toolchain or portable runtime | Pending | Pending | Experimental |
| Alpine / other musl systems | Separate native build | Not implemented/qualified as a release target | Pending | Unsupported release target |
| Flatpak / Snap app packages | Store/runtime packaging | Not implemented as a release channel | Portal/permission parity pending | Unsupported release channel |

The integrated 0.5.3 shared-staging engine passed the complete five-userspace suite. After the final legacy-manifest compatibility fix, Ubuntu passed the complete suite again and the other four userspaces passed its exact real HTTP resume regression. Their complete suites were not repeated after that fix. Build source hashes and package receipts are in [integration validation](linux-validation.md#integrated-engine-version-053).

For each accepted row, attach `linux:qualify` JSON with artifact SHA-256, source commit, image/snapshot/date, architecture/libc, GTK/WebKitGTK, desktop/session, browser version/package format, filesystem/mount and driver. Add package install/upgrade/remove logs and manual results. Record unavailable host services separately from unimplemented features or failed tests. Do not replace a pending result with a capability explanation.

## Manual acceptance required before certification

- Install, launch, upgrade, repair and remove each proposed package on a clean machine. Verify dependency resolution, executable modes, ABI, CA trust, icon/desktop entry, `.torrent`/magnet launch and preserved user data. Exercise AppImage FUSE and persistent extracted fallback; replace writable AppImage with valid signed updates and reject altered/wrong-version artifacts.
- Check actual WebKitGTK dialogs, clipboard, fonts, zoom, theme, progress/confirm/torrent windows, keyboard navigation, mixed DPI and representative GPUs in X11 and Wayland. Close and reopen with a working tray, with no watcher, and after a watcher disappears.
- Start at login, move/update the outer executable, and repair/unregister browser hosts. Verify cold/hot native messaging, exact extension IDs, stdout framing, actual Firefox ESR/release and Chromium-family downloads, authentication, batch behavior and fallback. Test default Snap/Flatpak browsers and their real portal authorization; a native-browser-only result is insufficient for confined-browser parity.
- Exercise NetworkManager connection selection and stale selection, logind authorization/inhibitors, denied/missing service errors and independent completion actions. Use a disposable machine for real shutdown/disconnect. Verify suspend/resume and external shutdown with active HTTP and torrent writes; manually paused items must remain paused.
- Run fixtures plus restart during connect/download/merge/publication. Check changed ETag/length, redirects, Retry-After, cancellation during cooldown, truncated/extra bytes, optional SHA-256, simultaneous names, Unicode paths, permissions, symlinks, low disk space and unwritable destinations. Test ext4/Btrfs, separate devices and representative NTFS/exFAT/network mounts.
- Measure useful end-to-end bytes/time, finalization/fsync, idle/startup, RSS/CPU/descriptors, binary/dependency download size, first/cold browser launch and representative 1/4/8/16/32-connection throughput. Use equal payloads/limits/storage, warm-up plus five runs, raw evidence and medians/tails. Include HTTP/torrent fairness and real client comparisons before claiming optimal tuning.

The original [support plan](linux-support-plan.md) remains the detailed backlog. Destination-direct writes, work stealing, sandboxed app channels, musl artifacts and deeper network/disk tuning remain qualification/measurement-dependent work; they are not advertised as completed.
