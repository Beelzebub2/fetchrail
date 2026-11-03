# Using Fetchrail

[← Back to the README](../README.md)

## Your first download

1. Paste an HTTP, HTTPS, FTP, FTPS, FTPES or SFTP file URL into Fetchrail.
2. Choose a destination, queue, or future start time when needed.
3. Start the download and expand its row to inspect the individual connections.

Pause a transfer to free a download slot, then resume it when you are ready. If Fetchrail closes during a transfer, the download returns paused on the next launch.

### Clipboard link suggestions (opt-in)

Enable **Settings → General → Suggest links copied to clipboard** if you want Fetchrail to recognize newly copied HTTP, HTTPS, FTP, SFTP or magnet links while the app is running. It is off by default. After enabling it, newly copied links appear in a small suggestion bar; select **Review links** to open the normal download confirmation window, or dismiss them. No transfer starts automatically. The existing clipboard contents are ignored when the setting is first enabled.

Only local clipboard text is examined. Raw clipboard contents and link history are neither persisted nor sent to any server or analytics. Long clipboard contents are skipped. On Linux, the monitor requires an available `wl-paste` (Wayland) or `xclip` (X11) executable and ordinary desktop clipboard access.

### Replace an expired download link

Pause a download, choose **More → Update download link**, and paste its replacement URL. By default, Fetchrail preserves the previous partial data separately and starts the new link from zero: a matching file size or ETag from another URL does **not** prove the bytes are the same.

If you supplied a **trusted Expected SHA-256** when creating the download, you can check **Attempt to reuse downloaded bytes**. On resume, Fetchrail probes the new link, confirms byte-range support and the same total size, and verifies the local saved range commitments. Only checkpointed bytes from the shared staging file may be reused. If any requirement fails, incompatible parts are quarantined and the new link starts cleanly. Even after reuse, the complete SHA-256 is checked before the file is published. Links containing temporary authorization parameters are never written into the part manifest.

The replacement link is entered manually; Fetchrail does not yet renew signed URLs from a website or account automatically.

## FTP, FTPS and SFTP files

Paste a direct `ftp://`, `ftps://`, `ftpes://` or `sftp://` file URL in **New downloads**. Files on these servers use the ordinary download list, folders, queues, schedules and speed controls. Only the desktop downloader accepts these URLs; the browser's session/extension handoff remains for HTTP and HTTPS. FTP/FTPS URLs can use an explicit username and password, and SFTP requires a username. SFTP can authenticate through a configured SSH agent or unencrypted default private key, or a password in the URL.

**FTP** is unencrypted; choose **FTPS** (FTP with TLS) or **SFTP** (SSH file transfer) when connecting with credentials or transferring sensitive content. FTPS requires valid TLS server certificates; `ftps://` and `ftpes://` use explicit TLS on port 21 by default, while port 990 uses implicit TLS. SFTP requires a trusted server SSH host key in `~/.ssh/known_hosts`. When possible, prefer SSH agent authentication because passwords embedded in links may be retained in local download state; Linux stores private state with restricted file permissions, while Windows protects application state with DPAPI.

HTTP servers can supply a strong ETag. FTP and SFTP file length and modification time are weaker signals, so Fetchrail normally uses one connection and starts over after an interruption. To enable parallel ranges and verified restart on a capable remote server, enter an independently trusted **Expected SHA-256** for that one file. The result is verified end-to-end before publication; a mismatched file cannot become a completed download.

FTP/FTPS and SFTP reuse file-size information from the initial probe instead of repeatedly querying it for every transfer segment. SFTP automatically pipelines reads for higher throughput on high-latency connections, while keeping its total read-ahead memory bounded across parallel connections. These optimizations do not change your download speed limits or the expected-hash requirement for safe resume.

## Torrents and magnets

Paste a magnet link or a public `.torrent` URL into the download bar, choose **Open torrent**, or drop a local `.torrent` file into the app. Magnets fetch metadata first. Payload downloading begins after you choose the files, destination and queue in the confirmation dialog. **Add paused** keeps the new job ready for later. Existing files require explicit verification before reuse.

HTTP and torrent transfers appear together. Use the **Torrents** and **Seeding** filters to narrow the list, and click a torrent's name to open its details. **Files** controls individual priorities and skipped files; **Peers**, **Trackers** and **Activity** show the swarm and engine status. The overview has sequential order, per-job download limits, file verification, reannounce and sharing goals.

Completed selected files continue sharing until a goal is reached. The defaults stop at ratio 1 or 24 active finished hours, whichever comes first. Set either goal to zero to remove that goal. Pausing stops both downloading and sharing. **Settings → Torrents** configures upload limits, sharing concurrency, discovery, TCP/uTP, encryption, interfaces and proxies. The global download cap and queue schedules apply to HTTP and torrent jobs together.

Pause a torrent before **Move storage**. **Remove…** offers an explicit payload-deletion checkbox; leaving it unchecked retains downloaded files. Unrelated files in the destination are preserved.

RSS, watch folders, search plugins and remote control are later features. See [torrent validation](torrent-validation.md) for delivered scope and measured tests.

## URL batches and speed limits

Use **New downloads** to paste several file URLs, one per line. The batch shares your folder, queue, start time, and per-file speed limit; rejected links stay in the form for correction.

### Verified multi-mirror HTTP downloads and Metalink (.meta4)

For a single HTTP(S) file, paste the primary URL in **New downloads**, add up to seven equivalent HTTP(S) addresses under **Alternative mirror URLs**, and supply a **trusted SHA-256** (64 hexadecimal characters) from the file publisher. Alternatively, **Import Metalink (.meta4)** from a file on your computer to prefill the name, size, checksum, and URLs, then inspect everything before selecting **Download**. Import reads only the XML bytes chosen in the dialog; it never opens arbitrary filesystem paths or starts transfers on its own. The importer supports one-file Metalink 4 documents with a SHA-256 hash and up to eight HTTP(S) URLs. It rejects malformed XML, DTD/custom entities, insecure filename paths, private literal host addresses, unsupported schemes, and manifests larger than 512 KiB.

Only verified-eligible mirrors are used. Fetchrail independently checks each alternate's size, HTTP byte-range support and its own strong representation validator; a mirror that cannot prove these properties is skipped. **An ETag from one website does not establish byte identity with another website**, even when the ETags look the same. The trusted SHA-256 is mandatory before combining ranges across websites, and the complete merged file must match it before any output is published. Each range is requested from one eligible source, with bounded retries that rotate to another eligible source when a mirror fails or changes. Existing queue, speed limits, proxy routing, pause/resume, verified staging and no-overwrite publication rules still apply. Expand a download's connection details to inspect numbered per-mirror eligibility, request counts and failures without exposing signed mirror URLs.

The primary URL must successfully pass its initial probe; optional mirrors cannot rescue a failed initial primary probe. Only HTTP(S) direct downloads are supported, not torrent, FTP/SFTP or browser-session handoffs. Matching sizes, validators and the checksum supplied inside a Metalink file do **not** authenticate the file's publisher: only import manifests from a source you trust. Cross-host transfers may be slower if mirror latency, bandwidth limits or failed probes outweigh the benefit. Local throughput measurements can be taken with the ignored Rust test `mirror_vs_single_source_throughput_baseline`; no fixed speed improvement is guaranteed. Private mirror URLs are kept out of ordinary download records and diagnostic events; the private saved transfer state retains the source list for restart recovery (protected with the existing Windows user-scoped state protection).

**Settings → Downloads** sets the total speed limit. Each download's **More → Speed limit** changes its cap while running. Limits use KiB/s; zero means unlimited.

Metalink's declared size is checked against the primary server's HTTP probe before any mirror ranges are combined. A size mismatch stops the transfer; the reported size is not treated as a substitute for a SHA-256 checksum.

### Desktop HTTP proxy and per-website request limits

In **Settings → Downloads → Network routing**, set the maximum number of simultaneous HTTP requests to a single website (1–32, default 32). This applies across all downloads, including redirect destinations, and does not bypass server `Retry-After` cooldowns. Reducing the limit causes new requests to wait until the currently active ones finish; transfers are not restarted.

Choose **HTTP proxy** or **SOCKS5 (remote DNS)** and enter a proxy host and port for the desktop's HTTP/HTTPS download engine. An explicit proxy applies to probes, each segmented connection, retries and browser download handoffs after the next full Fetchrail restart. If the proxy is unreachable, the request fails instead of falling back to a direct connection. Changes are **saved immediately but require restarting the application**; until then both new and active transfers continue using the previous route. **System/default routing** retains the app's existing environment/system proxy behavior. No proxy credentials or proxy URLs are stored in download history or logs.

Authentication to HTTP/SOCKS proxies is not implemented in this setting; do not put usernames, passwords or URLs into the host field. The existing torrent proxy controls operate independently, while FTP/FTPS/SFTP use separate transports and do not use this HTTP proxy. This is not a network kill switch or a verified VPN leak prevention control.

## Queues and schedules

In **Settings → Queues**, save a start time and optional stop time for a queue. At the stop time, active network transfers return to the queue with their partial bytes intact. Clear or extend the window to continue them.

Expand **Weekly recurring hours** under a queue to choose weekdays and time windows. A window starting Monday at 22:00 and ending at 02:00 also allows Tuesday from midnight to 02:00. Add up to 14 windows per queue, or remove every window to return to unrestricted weekly operation. These rules use the computer's **current local timezone** and automatically follow daylight-saving clock changes; a skipped local hour does not occur and a repeated local hour is included twice. Manual queue pauses and one-time start/stop restrictions still take precedence. A closed window pauses active transfers while preserving partial bytes; new jobs wait until the next open window.

Under a download's **More → Download priority**, choose Low, Below normal, Normal, High or Urgent. Priority applies when a new transfer slot is available; it does not interrupt another download or change the global connection limit. Older waiting jobs gain one level per hour until they match Urgent, after which the oldest wins. This prevents permanent starvation without allowing one item to exceed normal resource limits.

Schedules use local time in the interface. Fetchrail must be running to execute them, including in the tray.

## File categories

### Safe ZIP extraction

For a completed single-file `.zip` download, choose **More → Extract ZIP** to extract into a uniquely named folder beside the archive. The original ZIP is never removed or overwritten. If an archive is invalid, encrypted, contains symlinks/special files, path traversal, duplicate paths, unsafe Windows names, or excessive decompression size/ratio, extraction stops without publishing a partial output folder. No programs from the ZIP are run.

Enable **Settings → Downloads → Automatically extract new ZIP downloads** to do this for *future* completed ZIPs. It is off by default, never retroactively extracts existing history, and is limited to ordinary single-file downloads (not multi-file torrents). Each attempt is recorded to prevent duplicate output after a restart; failed attempts can be retried manually. Long-running extraction can leave a private staging folder after an unexpected process termination, but never replaces the original archive.

The built-in extractor supports ZIP only. RAR, 7z, password-protected archives, and ZIPs using unsupported compression methods require an external extractor. Extraction is bounded to 10,000 entries, a 16 GiB ZIP source, 16 GiB per member, 64 GiB expanded total and a conservative decompression ratio, to reduce resource exhaustion from malicious archives.

Files are sorted into **Compressed**, **Documents**, **Music**, **Programs**, and **Video** folders by default. Customize their extensions and destinations in **Settings → File categories**. Files without a matching category use the default download folder. When two categories list the same ending, the one higher in the list gets the file.

Each category keeps to one line with a count of its endings. Select the endings to edit them: type or paste several at once separated by spaces, commas or new lines (a leading `.` or `*.` is dropped and duplicates are skipped), filter a long list, or remove single endings, with **Undo** for the last removal. **Ctrl+S** saves settings while they are open.

Switch off **Automatically sort new downloads** to keep new files directly in the default Downloads folder. Explicitly selected save locations still take precedence. To sort files already loose in Downloads, select **Organize existing files** after saving your category rules. To get rid of category folders under Downloads, turn off automatic sorting, save, then select **Move files back to Downloads**. Both actions leave active/unfinished files and other subfolders alone, never overwrite existing files (name collisions get a numbered suffix), and update the saved locations of Fetchrail downloads. Empty category folders are removed when files are moved back; external category folders remain untouched.

## Browser downloads

Fetchrail includes companions for **Chrome, Chromium, Vivaldi, Brave, and Firefox**. **Settings → Browser** offers the store for your default browser: **Get from Firefox Add-ons** when it is Firefox, otherwise **Get from Chrome Web Store** (Chrome, Edge, Brave, Vivaldi and other Chromium-based browsers). Choose **Using another browser?** to see both. You can also follow the [browser companion guide](../browser-extension/README.md#install-the-companion) to load the bundled copy yourself.

Open the companion to paste URL batches, choose links from **This page**, or control live transfers. You can also right-click a link and choose **Download with Fetchrail**.

**Catch browser downloads** is enabled by default. The companion verifies a transfer before cancelling the browser's copy; a failed handoff continues in the browser. For a caught download, **Download file info** lets you confirm its name, category, and folder before starting or keeping it for later.

For sites with several buttons or countdowns, open the companion's **Options** and set **Links point to** to **Download pages**. See the guide for [page batches and session support](../browser-extension/README.md#multi-step-download-pages) and [routing limits](../browser-extension/README.md#routing-limits).

## Interface size

On large screens the main window grows its interface automatically, to about 120% on a 1440p display. To choose a size yourself, use **Settings → General → Interface size**, or press **Ctrl+=** and **Ctrl+-**; **Ctrl+0** returns to the automatic size. The choice is kept on this computer.

## Progress windows

**Start download** opens a separate progress window with live speed, time left, resume support, and individual connections. Reopen it through a download's **More → Download progress** menu. Closing the progress window keeps the transfer running.

- **Speed limiter** controls that download's bandwidth.
- **Options on completion** can show the completed window, exit Fetchrail, or use the shutdown and disconnect actions available on your system. Linux actions depend on desktop services and authorization; see [desktop behavior](linux.md#desktop-behavior).

## Installation, updates, and removal

Fetchrail runs on Windows and Linux. Choose the package for your system and CPU; see the [Linux guide](linux.md) for package availability, source builds and current qualification coverage.

Existing Braid installations retain their history, queues, settings, and partial downloads.

### Windows

Setup installs for your Windows account without an administrator prompt. The default location is `%LOCALAPPDATA%\Programs\Fetchrail`. The app requires the Microsoft Edge WebView2 Runtime.

Run a newer setup to update an existing installation, or let Fetchrail download signed updates in the background. Updates take effect on the next launch; **Restart to update** applies them immediately. Automatic updates and a manual **Check for updates** button are in **Settings → Background behavior**.

To uninstall, use **Windows Settings → Apps → Installed apps → Fetchrail**. Downloaded files stay on disk; removing history and settings is optional.

### Linux

Use a `.deb` for Ubuntu, Mint, Kali or Debian, an `.rpm` for Fedora, or the Arch PKGBUILD. AppImages and native archives cover other compatible glibc desktops. The [Linux installation guide](linux.md#choose-an-artifact) lists runtime requirements and installation commands.

Writable AppImages use signed automatic updates. Native packages update through their package manager; native archives and extracted AppImages use manual updates.

Remove a native package through its package manager. For portable installations, follow the [Linux removal instructions](linux.md#removal-and-troubleshooting). Downloaded files and saved data remain unless you explicitly remove them.

## Optional improvement analytics

Analytics is off until you choose **Allow**. **No thanks** or closing the prompt keeps it off, with all features available. **Settings → Privacy** shows exactly what is collected and lets you withdraw immediately, clearing unsent counts. Reports contain only broad platform/version information and combined transfer/feature counters and size buckets; they exclude URLs, filenames, credentials and identifiers. See [analytics and privacy details](analytics.md). Automatic update checks are a separate setting.

## Performance and diagnostics

For download calibration and automatic tuning, open **Settings → Performance → Optimize downloads**.
**Start quick test** runs with sensible defaults and lists the time, data and disk space it uses.
Under **Advanced test options**, choose Quick or Deep, whether to include online tests, and explicit network/temporary-space
budgets. Pause active downloads and seeding first. A completed, usable report can enable
automatic tuning; your connection ceiling and explicit per-download overrides remain.
The app learns lightly from uncapped, uncontaminated completed downloads and rejects stale
profiles. Tests, detailed reports and learning stay local. You can cancel, export a redacted
report or reset tuning. See [measurements, research and limits](performance-calibration.md).
Diagnostics and Performance tests cannot run together.

Open **Settings → Performance → Hardware diagnostics**, explicitly pause queued/scheduled/active transfers and seeders, and choose **Run diagnostics**. CPU, drive and memory tests measure real work; the optional connection test contacts Cloudflare with a bounded 128 MiB requested-payload budget. Disable it for local-only tests. No transfer is paused automatically. Cancellation keeps the last valid report and removes the uniquely owned fixture when OS I/O finishes.

The report shows measured numbers, short-sample warnings, provisional grades and conditional download/copy estimates. It does not assume unpacking. Full results stay local; **Preview optional summary** explains a separate per-run Share choice for coarse bands. The ordinary analytics switch does not authorize diagnostic sharing. See [methods and limits](diagnostics.md).
