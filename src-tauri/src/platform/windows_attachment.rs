use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use tokio::{fs, io::AsyncWriteExt};
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub(crate) async fn mark_download(
    path: &Path,
    url: &str,
    file_name: &str,
    check_with_windows: bool,
    health: Option<std::sync::Arc<crate::health::Health>>,
    #[cfg(feature = "test-tools")] trace: Option<std::sync::Arc<crate::engine::trace::Trace>>,
) -> io::Result<()> {
    #[cfg(feature = "test-tools")]
    let benchmark_source = (trace.is_some()
        && crate::platform::test_root()
            .ok()
            .flatten()
            .is_some_and(|root| path.starts_with(root)))
    .then(|| std::env::var("FETCHRAIL_TEST_ATTACHMENT_SOURCE").ok())
    .flatten();
    #[cfg(feature = "test-tools")]
    let url = benchmark_source.as_deref().unwrap_or(url);
    #[cfg(windows)]
    {
        if check_with_windows {
            let scan_path = path.to_path_buf();
            let source = url.to_string();
            let file_name = file_name.to_string();
            let scan_health = health.clone();
            #[cfg(feature = "test-tools")]
            let scan_trace = trace.clone();
            tokio::task::spawn_blocking(move || -> io::Result<()> {
                use windows::{
                    core::{GUID, PCWSTR},
                    Win32::{
                        System::Com::{
                            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                            COINIT_APARTMENTTHREADED,
                        },
                        UI::Shell::{AttachmentServices, IAttachmentExecute},
                    },
                };
                unsafe {
                    let setup_health_span = scan_health
                        .as_deref()
                        .map(|health| health.guard(crate::health::Phase::AttachmentSetup));
                    #[cfg(feature = "test-tools")]
                    let setup_span = scan_trace
                        .as_deref()
                        .map(|trace| trace.guard(crate::engine::trace::Phase::AttachmentSetup));
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                        .ok()
                        .map_err(io::Error::other)?;
                    struct Com;
                    impl Drop for Com {
                        fn drop(&mut self) {
                            unsafe {
                                CoUninitialize();
                            }
                        }
                    }
                    let _com = Com;
                    let attachment: IAttachmentExecute =
                        CoCreateInstance(&AttachmentServices, None, CLSCTX_INPROC_SERVER)
                            .map_err(io::Error::other)?;
                    let path_wide = wide(&scan_path);
                    let mut parsed = url::Url::parse(&source).map_err(io::Error::other)?;
                    let _ = parsed.set_username("");
                    let _ = parsed.set_password(None);
                    parsed.set_query(None);
                    parsed.set_fragment(None);
                    let source_wide: Vec<u16> =
                        parsed.as_str().encode_utf16().chain(Some(0)).collect();
                    attachment
                        .SetClientGuid(&GUID::from_u128(0xa02d0d22_8f98_4a53_b510_92a6897cdfd5))
                        .map_err(io::Error::other)?;
                    attachment
                        .SetLocalPath(PCWSTR(path_wide.as_ptr()))
                        .map_err(io::Error::other)?;
                    let name_wide: Vec<u16> = file_name.encode_utf16().chain(Some(0)).collect();
                    attachment
                        .SetFileName(PCWSTR(name_wide.as_ptr()))
                        .map_err(io::Error::other)?;
                    attachment
                        .SetSource(PCWSTR(source_wide.as_ptr()))
                        .map_err(io::Error::other)?;
                    #[cfg(feature = "test-tools")]
                    drop(setup_span);
                    drop(setup_health_span);
                    let _save_health_span = scan_health
                        .as_deref()
                        .map(|health| health.guard(crate::health::Phase::AttachmentSave));
                    #[cfg(feature = "test-tools")]
                    let _save_span = scan_trace
                        .as_deref()
                        .map(|trace| trace.guard(crate::engine::trace::Phase::AttachmentSave));
                    attachment.Save().map_err(io::Error::other)?;
                }
                Ok(())
            })
            .await
            .map_err(io::Error::other)??;
        }
        // Always preserve internet provenance, including when the user skips attachment approval.
        let ads = PathBuf::from(format!("{}:Zone.Identifier", path.display()));
        let ads_open_health_span = health
            .as_deref()
            .map(|health| health.guard(crate::health::Phase::AttachmentAdsOpen));
        #[cfg(feature = "test-tools")]
        let ads_open_span = trace
            .as_deref()
            .map(|trace| trace.guard(crate::engine::trace::Phase::AttachmentAdsOpen));
        let result = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(ads)
            .await;
        #[cfg(feature = "test-tools")]
        drop(ads_open_span);
        drop(ads_open_health_span);
        let mut file = match result {
            Ok(file) => file,
            Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 123)) => {
                eprintln!("This filesystem cannot store the download's Zone.Identifier.");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let public_url = url::Url::parse(url)
            .ok()
            .map(|mut url| {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            })
            .unwrap_or_default();
        {
            let _write_health_span = health
                .as_deref()
                .map(|health| health.guard(crate::health::Phase::AttachmentAdsWrite));
            #[cfg(feature = "test-tools")]
            let _write_span = trace
                .as_deref()
                .map(|trace| trace.guard(crate::engine::trace::Phase::AttachmentAdsWrite));
            file.write_all(
                format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={public_url}\r\n").as_bytes(),
            )
            .await?;
        }
        {
            let _sync_health_span = health
                .as_deref()
                .map(|health| health.guard(crate::health::Phase::AttachmentAdsSync));
            #[cfg(feature = "test-tools")]
            let _sync_span = trace
                .as_deref()
                .map(|trace| trace.guard(crate::engine::trace::Phase::AttachmentAdsSync));
            file.sync_all().await?;
        }
    }
    #[cfg(not(windows))]
    let _ = (path, url, file_name, check_with_windows);
    Ok(())
}
