use crate::engine::DownloadManager;
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use zbus::{zvariant::OwnedFd, Connection, Proxy};

pub async fn watch(manager: Arc<DownloadManager>) {
    if let Err(error) = monitor(manager).await {
        eprintln!("Fetchrail desktop power monitoring: {error}");
    }
}

async fn inhibitor(proxy: &Proxy<'_>) -> Option<OwnedFd> {
    proxy
        .call(
            "Inhibit",
            &(
                "sleep:shutdown",
                "Fetchrail",
                "Save transfer checkpoints",
                "delay",
            ),
        )
        .await
        .ok()
}

async fn monitor(manager: Arc<DownloadManager>) -> Result<(), String> {
    let bus = tokio::time::timeout(Duration::from_secs(4), Connection::system())
        .await
        .map_err(|_| "System bus connection timed out.")?
        .map_err(|e| e.to_string())?;
    let proxy = Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut sleep = proxy
        .receive_signal("PrepareForSleep")
        .await
        .map_err(|e| e.to_string())?;
    let mut shutdown = proxy
        .receive_signal("PrepareForShutdown")
        .await
        .map_err(|e| e.to_string())?;
    let mut delay = inhibitor(&proxy).await;
    let mut paused = Vec::new();
    loop {
        tokio::select! {
            message = sleep.next() => {
                let Some(message) = message else { return Ok(()); };
                let (preparing,): (bool,) = message.body().deserialize().map_err(|e| e.to_string())?;
                if preparing {
                    paused = manager.suspend_transfers().await;
                    delay.take();
                } else {
                    manager.resume_after_suspend(std::mem::take(&mut paused)).await;
                    delay = inhibitor(&proxy).await;
                }
            },
            message = shutdown.next() => {
                let Some(message) = message else { return Ok(()); };
                let (preparing,): (bool,) = message.body().deserialize().map_err(|e| e.to_string())?;
                if preparing { paused = manager.suspend_transfers().await; delay.take(); }
                else { manager.resume_after_suspend(std::mem::take(&mut paused)).await; delay = inhibitor(&proxy).await; }
            }
        }
    }
}
