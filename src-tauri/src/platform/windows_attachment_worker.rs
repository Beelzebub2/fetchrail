//! Run attachment policy in a disposable process. A cancelled, timed out,
//! crashed, or malformed worker never grants approval to publish a download.
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    os::windows::{ffi::OsStrExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use windows::Win32::{Foundation::HANDLE, System::Pipes::PeekNamedPipe};

const WORKER_ARG: &str = "--attachment-worker";
const MAX_REQUEST: u64 = 64 * 1024;
const MAX_REPLY: usize = 4096;
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
static WORKERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    path: PathBuf,
    source: String,
    file_name: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase", deny_unknown_fields)]
enum Reply {
    Save {},
    Approved {},
    Failed { message: String },
}

#[cfg(feature = "test-tools")]
struct SupervisionFixture {
    root: PathBuf,
    cancel: bool,
}
#[cfg(feature = "test-tools")]
impl SupervisionFixture {
    fn read(request: &Request) -> io::Result<Option<Self>> {
        if !std::env::var("FETCHRAIL_TEST_ATTACHMENT_SUPERVISION").is_ok_and(|value| value == "1") {
            return Ok(None);
        }
        let root = crate::platform::test_root()
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("Attachment fixture requires an isolated test root"))?
            .canonicalize()?;
        let path = request.path.canonicalize()?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(io::Error::other(
                "Attachment fixture candidate must be inside its test root",
            ));
        }
        let control = root.join("attachment-supervision-fixture");
        let metadata = std::fs::symlink_metadata(&control)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 {
            return Err(io::Error::other("Invalid attachment fixture control"));
        }
        let cancel = match std::fs::read_to_string(control)?.trim() {
            "off" => return Ok(None),
            "cancel" => true,
            "timeout" => false,
            _ => return Err(io::Error::other("Invalid attachment fixture control")),
        };
        Ok(Some(Self { root, cancel }))
    }
    fn record_child(&self) -> io::Result<()> {
        let name = if self.cancel {
            "attachment-worker-cancel.pid"
        } else {
            "attachment-worker-timeout.pid"
        };
        let mut marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(name))?;
        writeln!(marker, "{}", std::process::id())
    }
}

fn check(request: &Request, mut saving: impl FnMut() -> io::Result<()>) -> io::Result<()> {
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
    let path: Vec<u16> = request
        .path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let source: Vec<u16> = request.source.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = request.file_name.encode_utf16().chain(Some(0)).collect();
    if [&path, &source, &name]
        .iter()
        .any(|value| value[..value.len() - 1].contains(&0))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Attachment request contains a null character",
        ));
    }
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
        attachment
            .SetClientGuid(&GUID::from_u128(0xa02d0d22_8f98_4a53_b510_92a6897cdfd5))
            .map_err(io::Error::other)?;
        attachment
            .SetLocalPath(PCWSTR(path.as_ptr()))
            .map_err(io::Error::other)?;
        attachment
            .SetFileName(PCWSTR(name.as_ptr()))
            .map_err(io::Error::other)?;
        attachment
            .SetSource(PCWSTR(source.as_ptr()))
            .map_err(io::Error::other)?;
        saving()?;
        attachment.Save().map_err(io::Error::other)?;
    }
    Ok(())
}

fn emit(reply: &Reply) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, reply)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

/// Must run before normal app, preview, native-host, or single-instance startup.
/// Only the parent process receives the bounded request and approval result.
pub fn run_attachment_worker() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return None;
    }
    if args.next().is_some() {
        return Some(2);
    }
    let result = (|| {
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_REQUEST + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_REQUEST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Attachment request is too large",
            ));
        }
        let request: Request = serde_json::from_slice(&bytes)?;
        #[cfg(feature = "test-tools")]
        let fixture = SupervisionFixture::read(&request)?;
        check(&request, || {
            #[cfg(feature = "test-tools")]
            if let Some(fixture) = &fixture {
                fixture.record_child()?;
            }
            emit(&Reply::Save {})?;
            #[cfg(feature = "test-tools")]
            if fixture.is_some() {
                // Simulate an unresponsive policy provider in the actual child.
                // Even if its parent fails to reap it, this path can never approve.
                std::thread::sleep(Duration::from_secs(30));
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Attachment fixture hold ended without approval",
                ));
            }
            Ok(())
        })?;
        emit(&Reply::Approved {})
    })();
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            let message: String = error.to_string().chars().take(512).collect();
            let _ = emit(&Reply::Failed { message });
            1
        }
    })
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Reap before releasing the approval slot, including caller cancellation.
        // The candidate must not be retried/published while its old scanner runs.
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[derive(Default)]
struct Protocol {
    saving: bool,
    approved: bool,
}
impl Protocol {
    fn accept(&mut self, reply: Reply) -> io::Result<bool> {
        match reply {
            Reply::Save {} if !self.saving && !self.approved => {
                self.saving = true;
                Ok(true)
            }
            Reply::Approved {} if self.saving && !self.approved => {
                self.approved = true;
                Ok(false)
            }
            Reply::Failed { message } if !self.approved => Err(io::Error::other(format!(
                "Windows attachment approval failed: {message}"
            ))),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid attachment worker response",
            )),
        }
    }
    fn finish(&self, successful_exit: bool) -> io::Result<()> {
        if successful_exit && self.saving && self.approved {
            Ok(())
        } else {
            Err(io::Error::other(
                "Windows attachment worker exited without approval",
            ))
        }
    }
}

fn supervise(
    request: Vec<u8>,
    cancel: &CancellationToken,
    timeout: Duration,
    mut saving: impl FnMut(),
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let mut child = ChildGuard(
        Command::new(std::env::current_exe()?)
            .arg(WORKER_ARG)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Attachment request pipe is unavailable"))?;
    let mut output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Attachment result pipe is unavailable"))?;
    // A child held during process startup must not make a full stdin pipe hide
    // cancellation. The writer unblocks when the supervised child exits/is killed.
    let writer = std::thread::Builder::new()
        .name("attachment-request".into())
        .spawn(move || input.write_all(&request))?;
    let deadline = Instant::now() + timeout;
    let mut pending = Vec::new();
    let mut total = 0;
    let mut protocol = Protocol::default();
    loop {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows attachment approval timed out; the file was not published",
            ));
        }
        let mut available = 0;
        let peek = unsafe {
            PeekNamedPipe(
                HANDLE(output.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        };
        if peek.is_ok() && available > 0 {
            if total + available as usize > MAX_REPLY {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Attachment worker response is too large",
                ));
            }
            let mut bytes = vec![0; available as usize];
            output.read_exact(&mut bytes)?;
            total += bytes.len();
            pending.extend(bytes);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let reply: Reply = serde_json::from_slice(&pending[..end])?;
                pending.drain(..=end);
                if protocol.accept(reply)? {
                    saving();
                }
            }
            // Consume all output before checking exit; approval can arrive at exit.
            continue;
        }
        if let Some(status) = child.0.try_wait()? {
            // A final reply may have arrived between the preceding peek and exit.
            let mut remaining = 0;
            if unsafe {
                PeekNamedPipe(
                    HANDLE(output.as_raw_handle()),
                    None,
                    0,
                    None,
                    Some(&mut remaining),
                    None,
                )
            }
            .is_ok()
                && remaining > 0
            {
                continue;
            }
            writer
                .join()
                .map_err(|_| io::Error::other("Attachment request writer failed"))??;
            if !pending.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Incomplete attachment worker response",
                ));
            }
            return protocol.finish(status.success());
        }
        if let Err(error) = peek {
            // Broken pipe alone is not approval; the child's status is authoritative.
            if error.code().0 as u32 != 0x8007_006d {
                return Err(io::Error::other(error));
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) async fn approve(
    path: &Path,
    url: &str,
    file_name: &str,
    cancel: &CancellationToken,
    health: Option<Arc<crate::health::Health>>,
    #[cfg(feature = "test-tools")] trace: Option<Arc<crate::engine::trace::Trace>>,
) -> io::Result<()> {
    let mut source = url::Url::parse(url).map_err(io::Error::other)?;
    let _ = source.set_username("");
    let _ = source.set_password(None);
    source.set_query(None);
    source.set_fragment(None);
    let request = Request {
        path: path.to_path_buf(),
        source: source.to_string(),
        file_name: file_name.to_string(),
    };
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() as u64 > MAX_REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Attachment request is too large",
        ));
    }
    #[cfg(feature = "test-tools")]
    let fixture = SupervisionFixture::read(&request)?;
    let timeout = APPROVAL_TIMEOUT;
    #[cfg(feature = "test-tools")]
    let timeout = if fixture.is_some() {
        Duration::from_secs(5)
    } else {
        timeout
    };
    let permit = tokio::select! {
        _ = cancel.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
        permit = WORKERS.clone().acquire_owned() => permit.map_err(io::Error::other)?,
    };
    let worker_cancel = cancel.child_token();
    let _cancel_on_drop = CancelOnDrop(worker_cancel.clone());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut setup = health
            .as_deref()
            .map(|h| h.guard(crate::health::Phase::AttachmentSetup));
        let mut save = None;
        #[cfg(feature = "test-tools")]
        let mut setup_trace = trace
            .as_deref()
            .map(|t| t.guard(crate::engine::trace::Phase::AttachmentSetup));
        #[cfg(feature = "test-tools")]
        let mut save_trace = None;
        let saving = || {
            drop(setup.take());
            save = health
                .as_deref()
                .map(|h| h.guard(crate::health::Phase::AttachmentSave));
            #[cfg(feature = "test-tools")]
            {
                drop(setup_trace.take());
                save_trace = trace
                    .as_deref()
                    .map(|t| t.guard(crate::engine::trace::Phase::AttachmentSave));
                if fixture.as_ref().is_some_and(|fixture| fixture.cancel) {
                    worker_cancel.cancel();
                }
            }
        };
        #[cfg(not(test))]
        let result = supervise(bytes, &worker_cancel, timeout, saving);
        // Rust test binaries do not enter the app's early helper dispatcher. Keep
        // the existing real-COM regression tests; release/preview always isolate.
        #[cfg(test)]
        let result = {
            let _ = (bytes, timeout);
            let mut saving = saving;
            if worker_cancel.is_cancelled() {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                check(&request, || {
                    saving();
                    Ok(())
                })
            }
        };
        if worker_cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        result
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_supervisor_never_starts_approval() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = supervise(Vec::new(), &cancel, APPROVAL_TIMEOUT, || {
            panic!("Cancelled worker entered Save")
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    }
    #[test]
    fn worker_exit_or_partial_protocol_never_grants_approval() {
        let mut protocol = Protocol::default();
        assert!(protocol.finish(true).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"event":"save","extra":true}"#).is_err());
        assert!(protocol.accept(Reply::Approved {}).is_err());
        assert!(protocol.accept(Reply::Save {}).unwrap());
        assert!(protocol.finish(true).is_err());
        assert!(!protocol.accept(Reply::Approved {}).unwrap());
        assert!(protocol.finish(false).is_err());
        assert!(protocol.finish(true).is_ok());
        assert!(protocol.accept(Reply::Approved {}).is_err());
    }
    #[test]
    fn explicit_denial_and_unrecognized_messages_fail_closed() {
        let mut protocol = Protocol::default();
        assert!(protocol
            .accept(Reply::Failed {
                message: "Policy rejected file".into()
            })
            .is_err());
        assert!(protocol.finish(true).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"event":"approved","extra":true}"#).is_err());
        assert!(serde_json::from_str::<Reply>(r#"{"event":"timeout"}"#).is_err());
    }
}
