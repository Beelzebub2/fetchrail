//! Opt-in clipboard link suggestions. The monitor never starts a transfer,
//! persists clipboard text, or reports it to diagnostics/analytics.

use crate::engine::DownloadManager;
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter};
use url::Url;

const MAX_CLIPBOARD_BYTES: usize = 8 * 1024;
const MAX_LINKS: usize = 10;

fn parse_links(text: &str) -> Vec<String> {
    if text.len() > MAX_CLIPBOARD_BYTES || text.contains('\0') {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for input in text.split_whitespace().take(100) {
        let candidate = input.trim_matches(['<', '>', '(', ')', '[', ']', '"', '\'', ',', ';']);
        if candidate.len() > 2048 {
            continue;
        }
        let Ok(url) = Url::parse(candidate) else {
            continue;
        };
        let allowed = matches!(
            url.scheme(),
            "http" | "https" | "ftp" | "ftps" | "ftpes" | "sftp"
        ) && url.host_str().is_some();
        let magnet = url.scheme() == "magnet"
            && url.query_pairs().any(|(key, value)| {
                key == "xt" && (value.starts_with("urn:btih:") || value.starts_with("urn:btmh:"))
            });
        if (allowed || magnet) && seen.insert(candidate.to_owned()) {
            links.push(candidate.to_owned());
        }
        if links.len() == MAX_LINKS {
            break;
        }
    }
    links
}

pub fn start(app: AppHandle, manager: Arc<DownloadManager>) {
    tauri::async_runtime::spawn(async move {
        let mut poll = tokio::time::interval(Duration::from_secs(2));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut previously_enabled = false;
        #[cfg(windows)]
        let mut previous_sequence = 0u32;
        #[cfg(target_os = "linux")]
        let mut previous_fingerprint: Option<[u8; 32]> = None;
        loop {
            poll.tick().await;
            if !manager.settings().await.clipboard_monitoring {
                previously_enabled = false;
                #[cfg(target_os = "linux")]
                {
                    previous_fingerprint = None;
                }
                continue;
            }
            #[cfg(windows)]
            {
                // GetClipboardSequenceNumber is cheap and avoids opening the
                // clipboard until its actual content has changed. On opt-in,
                // suppress what the user copied *before* enabling monitoring.
                let sequence = windows_sequence();
                if !previously_enabled {
                    previously_enabled = true;
                    previous_sequence = sequence;
                    continue;
                }
                if sequence == 0 || sequence == previous_sequence {
                    continue;
                }
                let text = tokio::task::spawn_blocking(windows_clipboard_text)
                    .await
                    .ok()
                    .flatten();
                let Some(text) = text else {
                    // Another process may temporarily own the clipboard. Retry
                    // its same revision instead of silently missing the copy.
                    continue;
                };
                previous_sequence = sequence;
                let links = parse_links(&text);
                if !links.is_empty() {
                    let _ = app.emit("fetchrail://clipboard-links", links);
                }
            }
            #[cfg(target_os = "linux")]
            {
                // Linux desktops use their own clipboard broker. Only a
                // working installed wl-paste/xclip adapter is queried; no
                // shell or user-controlled command string is executed.
                let Some(text) = linux_clipboard_text().await else {
                    continue;
                };
                let fingerprint: [u8; 32] = Sha256::digest(text.as_bytes()).into();
                if !previously_enabled {
                    previously_enabled = true;
                    previous_fingerprint = Some(fingerprint);
                    continue;
                }
                if previous_fingerprint == Some(fingerprint) {
                    continue;
                }
                previous_fingerprint = Some(fingerprint);
                let links = parse_links(&text);
                if !links.is_empty() {
                    let _ = app.emit("fetchrail://clipboard-links", links);
                }
            }
        }
    });
}

#[cfg(windows)]
fn windows_sequence() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

#[cfg(windows)]
fn windows_clipboard_text() -> Option<String> {
    use windows::Win32::{
        Foundation::HGLOBAL,
        System::{
            DataExchange::{
                CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        },
    };

    // CF_UNICODETEXT; use the numeric standard clipboard format to avoid an
    // unrelated OLE dependency just for its public constant.
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(None).ok()?;
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = unsafe { CloseClipboard() };
            }
        }
        let _guard = Guard;
        IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let global = HGLOBAL(handle.0);
        let utf16_units = GlobalSize(global) / std::mem::size_of::<u16>();
        if utf16_units == 0 || utf16_units > MAX_CLIPBOARD_BYTES / 2 {
            return None;
        }
        let ptr = GlobalLock(global).cast::<u16>();
        if ptr.is_null() {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr, utf16_units);
        let end = slice
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(slice.len());
        let result = String::from_utf16(&slice[..end]).ok();
        let _ = GlobalUnlock(global);
        result
    }
}

#[cfg(target_os = "linux")]
async fn linux_clipboard_text() -> Option<String> {
    use std::process::Stdio;
    use tokio::{io::AsyncReadExt, process::Command};

    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let mut command = if wayland {
        let mut cmd = Command::new("wl-paste");
        cmd.args(["--no-newline", "--type", "text/plain"]);
        cmd
    } else {
        let mut cmd = Command::new("xclip");
        cmd.args(["-selection", "clipboard", "-out"]);
        cmd
    };
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let output = tokio::time::timeout(Duration::from_millis(1500), async {
        let mut bytes = Vec::new();
        let mut reader = child.stdout.take()?.take((MAX_CLIPBOARD_BYTES + 1) as u64);
        reader.read_to_end(&mut bytes).await.ok()?;
        let status = child.wait().await.ok()?;
        (status.success() && bytes.len() <= MAX_CLIPBOARD_BYTES)
            .then(|| String::from_utf8(bytes).ok())
            .flatten()
    })
    .await;
    match output {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill().await;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_requires_plain_bounded_download_links() {
        assert_eq!(
            parse_links(
                "https://files.test/a.zip\nhttps://files.test/a.zip\nftp://files.test/b.iso"
            ),
            ["https://files.test/a.zip", "ftp://files.test/b.iso"]
        );
        assert_eq!(
            parse_links("copy (https://files.test/b.pdf) now"),
            ["https://files.test/b.pdf"]
        );
        assert!(parse_links("file:///secret\njavascript:alert(1)\ndata:text/html,test").is_empty());
        assert!(parse_links(&"a".repeat(MAX_CLIPBOARD_BYTES + 1)).is_empty());
        assert!(parse_links("https://files.test/a.zip\0token").is_empty());
    }

    #[test]
    fn parser_preserves_signed_parameters_only_for_explicit_review() {
        let original = "https://files.test/a.zip?signature=private-token#fragment";
        assert_eq!(parse_links(original), [original]);
        let magnet = format!("magnet:?xt=urn:btih:{}", "a".repeat(40));
        assert_eq!(parse_links(&magnet), [magnet]);
        assert!(parse_links("magnet:?dn=filename").is_empty());
    }
}
