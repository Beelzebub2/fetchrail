use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::{fs, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;
#[path = "windows_attachment_worker.rs"]
mod worker;
pub use worker::run_attachment_worker;

pub(crate) async fn mark_download(
    path: &Path,
    url: &str,
    file_name: &str,
    check_with_windows: bool,
    cancel: &CancellationToken,
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
            worker::approve(
                path,
                url,
                file_name,
                cancel,
                health.clone(),
                #[cfg(feature = "test-tools")]
                trace.clone(),
            )
            .await?;
        }
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
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
