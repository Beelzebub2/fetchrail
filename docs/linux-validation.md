# Linux port validation

Recorded 9–10 October 2026. The x64 app builds and runs in Ubuntu, Fedora, Mint, Kali and Arch Linux userspaces. Docker was excluded at the user's request. An isolated Ubuntu 22.04.5 WSL2 installation supplies the Linux compiler, ext4 filesystem and kernel; four other verified official images run as non-root users in separate native chroots. These results do not certify their own kernels, security policies, hardware or full desktop sessions.

Before contributor integration, Windows passed 44 Rust tests and Linux passed 46, with one completion benchmark excluded from each normal suite. Both release builds passed. Windows' embedded WebView2 interface, signature rejection fixtures, real HTTP/native messaging and torrent swarm checks passed. The existing Braid installation was preserved: the setup/removal fixture intentionally refuses to exercise this account, while clean Windows CI includes that check.

The Linux build uses GCC 11, glibc 2.35, GTK 3.24.33 and WebKitGTK 2.50.4. The executable's highest required GLIBC symbol is `GLIBC_2.34`. Installed copies resolved their ELF libraries and passed real HTTP/native bridge transfers, queue windows, global/per-file limits, authentication, range rejection, filename collisions, SHA-256, pause/resume and process restart. Real v1/v2/hybrid TCP/uTP torrent fixtures passed, including selected files, duplicate rejection, magnet metadata and partial recovery. Chromium and Firefox extension code ran against the real host/engine with fallback checks; actual browser installations and confined browsers need separate acceptance.

| Userspace | Tested package | glibc | GTK3 | WebKitGTK 4.1 |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04.5 | Debian, AppImage, native archive | 2.35 | 3.24.33 | 2.50.4 |
| Fedora 44 | RPM installed through DNF | 2.43 | 3.24.52 | 2.54.1 |
| Mint 22.3 Xfce ISO, updated required libraries | Debian installed through APT | 2.39 | 3.24.41 | 2.52.6 |
| Kali 2026.2 WSL image, last-snapshot repositories | Debian installed through APT | 2.43 | 3.24.52 | 2.52.6 |
| Arch 2026.10.01 image, complete repository upgrade | Binary PKGBUILD, makepkg and pacman | 2.44 | 3.24.52 | 2.54.1 |

Native WebKitGTK controls passed in X11 and headless Wayland for all five: settings, autostart enable/remove, theme changes, a file's exact hash, completed progress, closing without deleting history and private tray icon storage. The current fixture also checks the removal dialog's cancel/confirm behavior and confirms that removing history preserves the completed file. Ubuntu also passed WSLg/Wayland. Distro Wayland/removal results are retained by the fixture and reported in the [matrix](linux-support-matrix.md). Screenshots accompany the JSON records. Physical GPUs, keyboard/IME, accessibility and full Cinnamon/Xfce/GNOME/KDE sessions remain acceptance work.

A new desktop launch regression reproduced the failure to import literal `file://` torrent URLs. Linux now decodes local file URLs and resolves relative filenames against the sending process's directory while preserving Windows argument handling. The native UI fixture checks real GLib desktop launch, literal file URLs with Unicode/spaces/percent/apostrophe characters, and relative second-instance launch; each requires the metadata confirmation dialog and leaves the download list empty until confirmation. Remote file hosts and ambiguous URL queries/fragments are rejected. A Mint run under concurrent fixture load also exposed an invalid resume-test comparison: live counters include partial blocks from multiple pieces. The corrected test compares hash-verified bytes for each selected file through libtorrent's existing piece-granularity details API and passed on Windows, Ubuntu and Mint. Production torrent code was unchanged by that test correction.

Unsigned x64 Debian, RPM, AppImage and native archives were generated and their exported hashes checked. Ubuntu package installation/removal, outer AppImage extract-and-run, persistent extracted AppImage cold browser-host launch from a path containing spaces, quotes and `%`, and native archive launch passed. The extracted launcher preserves AppRun's library environment. Production bundles assert that `torrent-harness` and `verify-update` are absent; these tools require the `test-tools` development feature. Debian/RPM downloads fell from approximately 16.21 MiB to 11.25 MiB; AppImage is approximately 85.4 MiB and the native archive 10.97 MiB. The [development inventory](../artifacts/linux-development/linux-x64-development.json) lists exact sizes and SHA-256 values. These artifacts are unsigned and produce no updater manifest.

Private D-Bus fixtures passed exact logind/NetworkManager messages, selected UUIDs, interactive authorization, policy denial and systemd-version gating. Missing optional desktop services did not prevent transfers. Frontend/extension/UI checks, private launcher/autostart integration, release aggregation, dependency notices and website platform selection passed. Ephemeral signing fixtures rejected modified artifacts, mismatched versions and forged comments; no release key was used.

One Kali forced-restart fixture emitted `free(): corrupted unsorted chunks` while its full suite still passed. A complete HTTP repeat under strace passed without that message, SIGABRT or SIGSEGV. The emitting process and cause remain unconfirmed; traces are retained in `artifacts/linux-qualification/kali`. This is an unresolved qualification diagnostic. Chroots also report unavailable system buses/GPUs and Glycin's sandbox fallback. No application sandbox was disabled. These environments cannot establish Fedora SELinux, Ubuntu AppArmor, production image-loader sandbox or physical desktop behavior. A separate Fedora WSL import failed at boot with `Wsl/Service/E_UNEXPECTED`; the Fedora results above use its verified image in the working Ubuntu VM.

Native Ubuntu x64/ARM64 CI includes builds, transfer tests, X11/Wayland controls and package checks. A further four-way job provisions checksum-pinned Fedora, Mint, Kali and Arch images without Docker and gates publication on installed-package, transfer, extension, X11/Wayland and removal checks. Runtime inputs are private CI artifacts; developer tools are excluded from release downloads. Baseline qualification did not dispatch CI, a release or a website deployment. ARM64's platform/update modules type-check, but a complete ARM64 app run, real sleep/shutdown/disconnection, signed AppImage replacement, default Snap/Flatpak browser handoff, NixOS and musl releases remain unqualified or unimplemented as recorded in the matrix.

Local evidence and review artifacts are under `artifacts/linux-qualification` and `artifacts/linux-development`, ignored by Git. The pre-integration app's actual built dirty source is captured in `startup-fixed-source.tar` with SHA-256 `ebd5cd2ac67069db1104be7bd8a5eecef2660e3f4f8ec513d29c97428f2ca62d`; its base commit is `0b2138b8ab1b7da495beb738fcd203ecba35f013`. Earlier `qualified-build-source.tar` (`6c643c0d06e855bd3d20e9eebacd1d7c8bc256da93a1c13fc43803b31e3ec5ca`) and `final-built-source.tar` (`dbb9ac454b6310e15a8d082ed39a47036fbedf29435719423c2eb14b5c3c9f5f`) remain for earlier qualification and performance provenance. Subsequent test-script/CI/documentation edits are separate from those binary snapshots. A base commit alone does not identify this shared checkout's binaries.

Concurrent startup requests exposed a shared native-bridge deadlock: the ping handler synchronously queried the webview URL while Tauri's setup was still using its async runtime. The locked Tauri implementation sends that query to the UI thread and waits for a blocking response. The ping handler now reads URL metadata cached by Tauri's own main-page-load callback, keeping frontend readiness independent of bridge availability. Sixteen simultaneous authenticated pings exceeded 30 seconds on the old Fedora build; all five current Linux userspaces and Windows pass the new five-second-per-ping regression, followed by actual embedded frontend rendering. Reproducer and debugger records are retained alongside both source snapshots.

The combined five-userspace run timed out once on Ubuntu's cookie-session restart completion and Arch's three-second queue window. Both complete transfer suites passed sequential repeats without increasing timeouts or changing the app; the cause remains unconfirmed and failed JSON records are retained. HTTP failures now save only fixture statuses/counters and synthetic server measurements. Mint's UI checks passed but its fixture cleanup failed when desktop portals left FUSE mounts under the temporary Wayland runtime. Cleanup now unmounts only those private paths, refuses to remove mounted directories, waits for portal shutdown, and preserves the original exit status. A native Mint Wayland repeat and package removal passed, as did a focused regression that deliberately prevents unmounting and verifies that mounted data survives. Fresh distro fixture cleanup also stops only processes rooted in its own temporary chroot; a regression confirms that an outside process survives.

## Completion performance

Each run downloaded a repeatable 512 MiB loopback representation, with one warm-up and five measured runs. Timing ends after finalization/file sync. Every output passes exact SHA-256 verification afterward, outside the timer for every client. Optimized Linux and Windows measurements ran separately, without simultaneous builds, distro installation or fixture suites. Curl/aria2 timings include their process startup; Fetchrail uses the release test executable. This measures local storage/loopback completion, not WAN speed or distro ranking.

| Environment / client | Connections | Median | Slowest measured run | Useful decimal MB/s |
| --- | --- | --- | --- | --- |
| Linux Fetchrail before optimization | 1 | 20.19 s | 22.26 s | 26.6 |
| Linux Fetchrail after optimization | 1 | 10.29 s | 11.52 s | 52.2 |
| Linux curl, same optimized run | 1 | 10.31 s | 13.66 s | 52.1 |
| Linux aria2, same optimized run | 1 | 6.94 s | 8.99 s | 77.3 |
| Linux aria2, same optimized run | 4 | 8.39 s | 9.92 s | 64.0 |
| Linux aria2, same optimized run | 8 | 7.44 s | 9.58 s | 72.1 |
| Windows Fetchrail after optimization | 1 | 6.75 s | 8.49 s | 79.6 |
| Windows curl, same optimized run | 1 | 5.15 s | 9.40 s | 104.2 |

The optimization hard-links a complete, validated single part to the temporary output when both paths share a filesystem, preserving resume bytes until safe publication. Hash checks, no-overwrite publication and final sync remain intact. Unsupported links or separate devices use the checked-copy fallback; Linux tested both inode reuse and a separate tmpfs destination, and Windows tested its own finalization behavior.

The observed Linux median was 49% lower. Storage variation prevents attributing that entire difference to the code: Linux curl's pre-change median was 5.74 s, compared with 10.31 s afterward. Aria2 remained faster in the optimized sample. The old Windows 7.13 s sample overlapped builds and is diagnostic, not a controlled baseline. Increasing Fetchrail connections in the pre-change Linux sample did not consistently improve disk-bound completion; multipart transfers at that baseline still used checked copies. The subsequent shared-staging integration is recorded in [download engine integration](download-engine-integration.md). No universal fastest-client claim is supported.

All 30 optimized Linux and 12 optimized Windows outputs matched their hashes. Raw records are [Linux before](../artifacts/linux-qualification/linux-before-optimization.json), [Linux after](../artifacts/linux-qualification/linux-after-optimization.json) and [Windows after](../artifacts/benchmarks/1791580632596-b74a9d14-f33d-4ee8-aa03-47a982b3b593.json). Destination-direct multipart writes, work stealing, WAN/storage measurements and broader hardware qualification remain evidence-dependent work in the [support plan](linux-support-plan.md).

## Integrated engine, version 0.5.3

The selected contributor improvements retain our UI, Linux platform adapters,
torrents, browser sessions and Windows behavior. The decisions and completion
measurements are recorded in [download engine integration](download-engine-integration.md).
The final source passed 55 Windows and 57 Linux native Rust tests; each suite
excludes one optional throughput benchmark. Both release builds passed. Regressions
cover shared staging, legacy partial files and merge fingerprints, damaged resume
prefixes, selective retries, representation changes, unknown lengths, cancellation,
shared cooldowns and trickling tails. Native HTTP fixtures use the developer's
Node.js runtime and flush a deliberately truncated prefix before half-closing its
socket. The application does not require Node.js at runtime.

The shared-staging implementation passed the complete installed-package suite
sequentially in all five userspaces:

| Userspace | Version 0.5.3 package | Transfer and extension fixtures | Native desktop controls | Removal |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04.5 | Debian, AppImage, native archive | Passed | X11 and Wayland, 13 checks each | Passed |
| Fedora 44 | RPM through DNF | Passed | X11 and Wayland, 13 checks each | Passed |
| Mint 22.3 | Debian through APT | Passed | X11 and Wayland, 13 checks each | Passed |
| Kali 2026.2 | Debian through APT | Passed | X11 and Wayland, 13 checks each | Passed |
| Arch, October 2026 | Binary PKGBUILD through makepkg/pacman | Passed | X11 and Wayland, 13 checks each | Passed |

That run's source archive is `linux-before-legacy-fix-source.tar`, SHA-256
`e1c32f48e030cddfc9f52f641970da030821168e136ec6809878ac340a4223ab`.
The last compatibility fix preserves an already valid manifest's original bytes,
including legacy JSON layouts, so a completed merge checkpoint keeps its original
fingerprint. The final source passed the complete Ubuntu suite again, plus the
exact legacy regression with real HTTP suffix recovery in Fedora, Mint, Kali and
Arch. The targeted helper now selects the current unit binary explicitly and
requires one passed test; selecting an older binary had produced zero-test output,
which was rejected and replaced by actual test runs. The complete four-distro
suite has not been repeated after this last fix.

Windows passed embedded WebView2/native messaging, the complete HTTP suite,
v1/v2/hybrid torrent swarms, 24 automatic listener checks, packaged torrent desktop
integration and signature rejection. The first final desktop fixture timed out
waiting for its local seeder to listen before the application started. A fresh
complete repeat passed without increasing timeouts; the cause is unconfirmed and
both logs are retained. Its mixed HTTP/torrent test delivered 2,134,178 bytes/s
over 60.02 seconds against a shared 2,097,152 bytes/s cap, within the fixture's
allowed burst tolerance. Existing installation state was preserved; setup/removal
remains a clean-runner CI check. Chromium/Firefox extension fixtures, release
aggregation, store packaging, download-progress controls and the large torrent
interface fixture passed.

The four final unsigned development packages were exported and independently
rehashed. The inventory is `artifacts/linux-development/linux-x64-development.json`.

| Package | Size, bytes | SHA-256 |
| --- | ---: | --- |
| Debian | 11,835,808 | `86c55cc0147a795bdb3a00814d2419e6ced9bc6367dd68ae966a3a9172910d20` |
| RPM | 11,836,250 | `db68ae78a5ef3d05d7cc9eaad246c7ba50b263d2872f65144667bbaeb11c9827` |
| AppImage | 89,524,728 | `cc5f0bd070feb7b493ad6c096b64548446ddd56a3013e9c702f95b2b96b446b0` |
| Native archive | 11,539,006 | `be3c2a6f68a247a9c6f6d672059a1554849b09bc0785beac6bb787877bf37b69` |

The final qualified Ubuntu installed executable has SHA-256
`f28980dac5e6a4ffe988c315ae9e43d69f2ae1f37c64979b9885a007a17af62c`; the Windows runtime executable has SHA-256
`af69ba170526a65f3253543261a98105f04c4a8cfafc03bdb21bb2227a5eb04f`. Both final build source archives,
`artifacts/integration/linux-built-source.tar` and `windows-built-source.tar`, have
SHA-256 `2604670b1e45aa3a0b6853610056d496254203b27cf275f14ec30394b4dcf8ee`. Later edits are documentation and seeder
fixture diagnostics. All snapshots remain local ignored artifacts.

The complete earlier five-distro receipts are in
`artifacts/integration/linux-final-results.json`. Final Ubuntu and targeted distro
evidence is summarized in `linux-guarded-results.json`; Windows evidence, including
the failed first fixture and passing repeat, is in `windows-final-results.json`.
Raw package, transfer and desktop records remain in `artifacts/linux-qualification`.
Prepared chroot cleanup stopped only task-owned services and removed their bind
mounts while preserving source, images and evidence.

All Linux runs share the WSL2 kernel and virtual desktops. Physical hardware,
complete distro security policies, actual installed/confined browsers, ARM64
execution and the other pending acceptance items above remain separate. Docker
was not used. Pushing `main` runs Windows, Linux x64/ARM64 and installed-distro CI
gates; release publication and store submission require a version tag.
