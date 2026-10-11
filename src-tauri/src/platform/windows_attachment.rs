use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use tokio::{fs, io::AsyncWriteExt};
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub async fn mark_download(path: &Path, url: &str, file_name: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let scan_path = path.to_path_buf();
        let source = url.to_string();
        let file_name = file_name.to_string();
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
                let source_wide: Vec<u16> = parsed.as_str().encode_utf16().chain(Some(0)).collect();
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
                attachment.Save().map_err(io::Error::other)?;
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        // Preserve internet provenance after attachment policy/scanning has accepted the staging file.
        let ads = PathBuf::from(format!("{}:Zone.Identifier", path.display()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(ads)
            .await
        {
            Ok(file) => file,
            Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 123)) => {
                eprintln!("Attachment policy accepted the file; this filesystem cannot store Zone.Identifier.");
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
        file.write_all(
            format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={public_url}\r\n").as_bytes(),
        )
        .await?;
        file.sync_all().await?;
    }
    #[cfg(not(windows))]
    let _ = (path, url, file_name);
    Ok(())
}
