//! The sole owner of the C++ engine lives on a dedicated thread. No native call
//! or alert/disk wait runs on the UI or Tokio executor threads.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

extern "C" {
    fn fr_torrent_new() -> *mut c_void;
    fn fr_torrent_drop(ptr: *mut c_void);
    fn fr_torrent_call(ptr: *mut c_void, input: *const c_char) -> *mut c_char;
    fn fr_torrent_call_cooperative(
        ptr: *mut c_void,
        input: *const c_char,
        service: extern "C" fn(*mut c_void),
        context: *mut c_void,
    ) -> *mut c_char;
    fn fr_torrent_create(
        input: *const c_char,
        progress: extern "C" fn(*mut c_void, u64, u64) -> bool,
        context: *mut c_void,
    ) -> *mut c_char;
    fn fr_torrent_free(ptr: *mut c_char);
}

fn invoke(
    request: Value,
    call: impl FnOnce(*const c_char) -> *mut c_char,
) -> Result<Value, String> {
    let input = CString::new(request.to_string()).map_err(|e| e.to_string())?;
    let result = call(input.as_ptr());
    if result.is_null() {
        return Err("Torrent engine ran out of memory".into());
    }
    // Every C++ entry returns a complete owned envelope and catches exceptions.
    let bytes = unsafe { CStr::from_ptr(result).to_bytes().to_vec() };
    unsafe { fr_torrent_free(result) };
    let envelope: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if let Some(error) = envelope.get("error") {
        return Err(error.as_str().unwrap_or("Torrent engine failed").into());
    }
    envelope
        .get("ok")
        .cloned()
        .ok_or_else(|| "Invalid native torrent response".into())
}
struct Native(*mut c_void);
impl Native {
    fn call(&self, request: Value) -> Result<Value, String> {
        invoke(request, |input| unsafe { fr_torrent_call(self.0, input) })
    }
    fn call_cooperative(&self, request: Value, queue: &mut ControlQueue) -> Result<Value, String> {
        let mut context = ServiceContext {
            native: self.0,
            queue,
        };
        invoke(request, |input| unsafe {
            // The native wait invokes this synchronously on this owner thread.
            fr_torrent_call_cooperative(
                self.0,
                input,
                service_controls,
                (&mut context as *mut ServiceContext<'_>).cast(),
            )
        })
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        unsafe { fr_torrent_drop(self.0) };
    }
}

type Request = (Value, Instant, oneshot::Sender<Result<Value, String>>);
const CONTROL_CAPACITY: usize = 128;
struct ControlQueue {
    receiver: mpsc::Receiver<Request>,
    deferred: VecDeque<Request>,
}
impl ControlQueue {
    fn new(receiver: mpsc::Receiver<Request>) -> Self {
        Self {
            receiver,
            deferred: VecDeque::new(),
        }
    }
    fn next(&mut self) -> Result<Request, mpsc::RecvError> {
        self.deferred
            .pop_front()
            .map_or_else(|| self.receiver.recv(), Ok)
    }
    fn live(request: &Request) -> bool {
        !request.2.is_closed() && Instant::now() < request.1
    }
    fn read_only(request: &Value) -> bool {
        matches!(
            request["op"].as_str(),
            Some("poll" | "details" | "pathAvailable")
        )
    }
    fn service(&mut self, mut call: impl FnMut(Value) -> Result<Value, String>) {
        self.deferred.retain(Self::live);
        for _ in 0..16 {
            // Bounded deferral preserves backpressure even through long disk waits.
            if self.deferred.len() >= CONTROL_CAPACITY {
                break;
            }
            let Ok(request) = self.receiver.try_recv() else {
                break;
            };
            if !Self::live(&request) {
                continue;
            }
            let safe = Self::read_only(&request.0)
                || (matches!(request.0["op"].as_str(), Some("pause" | "limit"))
                    && self
                        .deferred
                        .iter()
                        .all(|queued| Self::read_only(&queued.0)));
            if safe {
                let _ = request.2.send(call(request.0));
            } else {
                // Mutations remain FIFO. In particular, a later pause must not
                // overtake an earlier queued resume that would undo it afterwards.
                self.deferred.push_back(request);
            }
        }
    }
}
struct ServiceContext<'a> {
    native: *mut c_void,
    queue: &'a mut ControlQueue,
}
extern "C" fn service_controls(context: *mut c_void) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // No concurrent owner or nested storage waits are admitted by service().
        let context = unsafe { &mut *context.cast::<ServiceContext<'_>>() };
        let native = context.native;
        context
            .queue
            .service(|request| invoke(request, |input| unsafe { fr_torrent_call(native, input) }));
    }));
}

const CREATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
struct CreationControl {
    id: uuid::Uuid,
    cancelled: AtomicBool,
    completed: AtomicU64,
    total: AtomicU64,
    accepted: Instant,
    deadline: Instant,
    #[cfg(test)]
    test_gate: Option<CreationTestGate>,
}
#[cfg(test)]
struct CreationTestGate {
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    used: AtomicBool,
}
impl CreationControl {
    fn new(timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            id: uuid::Uuid::new_v4(),
            cancelled: AtomicBool::new(false),
            completed: AtomicU64::new(0),
            total: AtomicU64::new(0),
            accepted: now,
            deadline: now + timeout,
            #[cfg(test)]
            test_gate: None,
        }
    }
    fn active(&self) -> bool {
        !self.cancelled.load(Ordering::Relaxed) && Instant::now() < self.deadline
    }
    fn snapshot(&self) -> Value {
        json!({"active":true, "id":self.id, "completedPieces":self.completed.load(Ordering::Relaxed),
            "totalPieces":self.total.load(Ordering::Relaxed), "cancelRequested":!self.active(),
            "elapsedMs":self.accepted.elapsed().as_millis().min(u64::MAX as u128) as u64})
    }
}
struct CancelCreationOnDrop(Arc<CreationControl>);
impl Drop for CancelCreationOnDrop {
    fn drop(&mut self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
    }
}
extern "C" fn creation_progress(context: *mut c_void, completed: u64, total: u64) -> bool {
    std::panic::catch_unwind(|| {
        // The Arc held by the dedicated worker outlives all native callbacks.
        let control = unsafe { &*context.cast::<CreationControl>() };
        #[cfg(test)]
        if let Some(gate) = &control.test_gate {
            if !gate.used.swap(true, Ordering::Relaxed) {
                let _ = gate.entered.try_send(());
                if gate
                    .release
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .recv_timeout(Duration::from_secs(10))
                    .is_err()
                {
                    control.cancelled.store(true, Ordering::Relaxed);
                }
            }
        }
        control.total.store(total, Ordering::Relaxed);
        control
            .completed
            .fetch_max(completed.min(total), Ordering::Relaxed);
        control.active()
    })
    .unwrap_or(false)
}
struct CreationRequest {
    request: Value,
    control: Arc<CreationControl>,
    reply: oneshot::Sender<Result<Value, String>>,
}
struct CreationWorker {
    sender: mpsc::SyncSender<CreationRequest>,
    current: Arc<Mutex<Option<Arc<CreationControl>>>>,
}
impl CreationWorker {
    fn new() -> Result<Self, String> {
        // One hash job and at most one queued job. It never owns or calls the
        // live libtorrent session, so hashing cannot block poll/pause/checkpoints.
        let (sender, receiver) = mpsc::sync_channel::<CreationRequest>(1);
        let current = Arc::new(Mutex::new(None));
        let worker_current = current.clone();
        std::thread::Builder::new()
            .name("torrent-create".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if job.reply.is_closed() || !job.control.active() {
                        continue;
                    }
                    *worker_current.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(job.control.clone());
                    let result = invoke(job.request, |input| unsafe {
                        fr_torrent_create(
                            input,
                            creation_progress,
                            Arc::as_ptr(&job.control).cast_mut().cast(),
                        )
                    });
                    *worker_current.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { sender, current })
    }
    async fn create(&self, request: Value) -> Result<Value, String> {
        let control = Arc::new(CreationControl::new(CREATION_TIMEOUT));
        let _cancel_on_drop = CancelCreationOnDrop(control.clone());
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(CreationRequest {
                request,
                control,
                reply,
            })
            .map_err(|e| format!("Torrent creation queue unavailable: {e}"))?;
        tokio::time::timeout(CREATION_TIMEOUT, result).await
            .map_err(|_| "Torrent creation timed out and cancellation was requested. Metadata publication already in progress may still finish; check the output before retrying.".to_string())?
            .map_err(|_| "Torrent creation worker stopped".to_string())?
    }
    fn status(&self) -> Value {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(|| json!({"active":false}), |control| control.snapshot())
    }
    fn cancel(&self, id: &str) -> Result<Value, String> {
        let id =
            uuid::Uuid::parse_str(id).map_err(|_| "Invalid torrent creation ID".to_string())?;
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let control = current
            .as_ref()
            .filter(|control| control.id == id)
            .ok_or("Torrent creation is no longer active")?;
        control.cancelled.store(true, Ordering::Relaxed);
        Ok(json!({"id":id,"cancelling":true}))
    }
}

pub struct TorrentEngine {
    sender: mpsc::SyncSender<Request>,
    creation: CreationWorker,
    pub recovered_unclean: bool,
}
impl TorrentEngine {
    pub fn new(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let root = root
            .to_str()
            .ok_or("Torrent state requires a UTF-8 path; choose a UTF-8 home/data directory.")?
            .to_owned();
        let (sender, receiver) = mpsc::sync_channel::<Request>(CONTROL_CAPACITY);
        let (ready, initialized) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("torrent-control".into())
            .spawn(move || {
                let native = Native(unsafe { fr_torrent_new() });
                if native.0.is_null() {
                    let _ =
                        ready.send(Err("Could not initialize native torrent engine".to_string()));
                    return;
                }
                let result = native
                    .call(json!({"op":"init", "root":root}))
                    .map(|value| value["unclean"].as_bool().unwrap_or(true));
                let failed = result.is_err();
                let _ = ready.send(result);
                if failed {
                    return;
                }
                let mut queue = ControlQueue::new(receiver);
                while let Ok(request) = queue.next() {
                    // Dropped/expired queued mutations never run after caller timeout.
                    if !ControlQueue::live(&request) {
                        continue;
                    }
                    let _ = request
                        .2
                        .send(native.call_cooperative(request.0, &mut queue));
                }
            })
            .map_err(|e| e.to_string())?;
        let recovered_unclean = initialized
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Torrent initialization timed out".to_string())??;
        Ok(Self {
            sender,
            creation: CreationWorker::new()?,
            recovered_unclean,
        })
    }
    pub async fn call(&self, request: Value) -> Result<Value, String> {
        // Inject only a submission rejection, never a native storage outcome.
        // The normal manager timer and health path must handle the returned error.
        #[cfg(feature = "test-tools")]
        if request["op"] == "checkpoint" && request["wait"] != true {
            if let Some(root) = crate::platform::test_root()? {
                let marker = root.join("reject-checkpoint-once");
                if std::fs::symlink_metadata(&marker)
                    .is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() <= 64)
                    && std::fs::read_to_string(&marker).is_ok_and(|contents| {
                        std::env::var("FETCHRAIL_TEST_ID").is_ok_and(|id| contents == id)
                    })
                    && std::fs::remove_file(&marker).is_ok()
                {
                    return Err("Injected torrent checkpoint submission failure.".into());
                }
            }
        }
        match request["op"].as_str() {
            Some("create") => return self.creation.create(request).await,
            Some("creationStatus") => return Ok(self.creation.status()),
            Some("cancelCreation") => {
                return self
                    .creation
                    .cancel(request["id"].as_str().unwrap_or_default())
            }
            _ => {}
        }
        let (reply, result) = oneshot::channel();
        let timeout = if matches!(request["op"].as_str(), Some("poll" | "details")) {
            Duration::from_secs(3)
        } else if matches!(request["op"].as_str(), Some("checkpoint" | "remove")) {
            Duration::from_secs(35)
        } else {
            Duration::from_secs(20)
        };
        self.sender
            .try_send((request, Instant::now() + timeout, reply))
            .map_err(|e| format!("Torrent control queue unavailable: {e}"))?;
        tokio::time::timeout(timeout, result).await
            .map_err(|_| "Torrent command timed out; its in-flight outcome is uncertain. Refresh before retrying a storage mutation.".to_string())?
            .map_err(|_| "Torrent worker stopped".to_string())?
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFile {
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub priority: u8,
    pub downloaded: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMetadata {
    pub file_count: usize,
    pub name: String,
    pub hashes: Vec<String>,
    pub private: bool,
    pub total_bytes: u64,
    pub files: Vec<TorrentFile>,
    pub piece_length: u64,
    pub pieces: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TorrentSummary {
    pub hashes: Vec<String>,
    pub uploaded_bytes: u64,
    pub all_downloaded_bytes: u64,
    pub upload_speed_bps: u64,
    pub peers: usize,
    pub seeds: usize,
    pub selected_ready: bool,
    pub active_seconds: u64,
    pub seed_seconds: u64,
    pub ratio_limit: f64,
    pub seed_time_limit: u64,
    pub sequential: bool,
    #[serde(skip_serializing)]
    pub priorities: std::sync::Arc<Vec<u8>>,
    pub metadata_path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TorrentSettings {
    pub upload_limit_bps: u64,
    pub max_seeds: usize,
    pub ratio_limit: f64,
    pub seed_time_limit: u64,
    pub dht: bool,
    pub lsd: bool,
    pub upnp: bool,
    pub connections: usize,
    pub listen_interfaces: String,
    pub outgoing_interfaces: String,
    pub encryption: u8,
    pub protocol: u8,
    pub proxy_type: u8,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_username: String,
    pub proxy_password: String,
}
impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            upload_limit_bps: 0,
            max_seeds: 5,
            ratio_limit: 1.0,
            seed_time_limit: 86400,
            dht: true,
            lsd: true,
            upnp: true,
            connections: 500,
            listen_interfaces: "0.0.0.0:0,[::]:0".into(),
            outgoing_interfaces: String::new(),
            encryption: 1,
            protocol: 0,
            proxy_type: 0,
            proxy_host: String::new(),
            proxy_port: 0,
            proxy_username: String::new(),
            proxy_password: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentImportRequest {
    pub source: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentCommitRequest {
    pub id: uuid::Uuid,
    pub directory: String,
    pub priorities: Vec<u8>,
    pub queue: String,
    pub start_paused: bool,
    #[serde(default)]
    pub verify_existing: bool,
    pub scheduled_for: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod control_tests {
    use super::*;

    fn enqueue(
        sender: &mpsc::SyncSender<Request>,
        op: &str,
    ) -> oneshot::Receiver<Result<Value, String>> {
        let (reply, result) = oneshot::channel();
        sender
            .try_send((
                json!({"op":op, "id":"fixture"}),
                Instant::now() + Duration::from_secs(10),
                reply,
            ))
            .unwrap();
        result
    }

    #[test]
    fn reads_bypass_storage_waits_without_reordering_mutations() {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let mut queue = ControlQueue::new(receiver);
        let mut resume = enqueue(&sender, "resume");
        let mut pause = enqueue(&sender, "pause");
        let mut poll = enqueue(&sender, "poll");
        let mut details = enqueue(&sender, "details");
        let mut available = enqueue(&sender, "pathAvailable");
        let mut serviced = Vec::new();
        queue.service(|request| {
            serviced.push(request["op"].as_str().unwrap().to_string());
            Ok(json!({}))
        });
        assert_eq!(serviced, ["poll", "details", "pathAvailable"]);
        assert!(poll.try_recv().unwrap().is_ok());
        assert!(details.try_recv().unwrap().is_ok());
        assert!(available.try_recv().unwrap().is_ok());
        assert!(resume.try_recv().is_err());
        assert!(pause.try_recv().is_err());
        assert_eq!(queue.next().unwrap().0["op"], "resume");
        assert_eq!(queue.next().unwrap().0["op"], "pause");
    }

    #[test]
    fn safe_controls_stay_responsive_and_dead_requests_never_execute() {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let mut queue = ControlQueue::new(receiver);
        let dropped = enqueue(&sender, "pause");
        drop(dropped);
        let (reply, _expired) = oneshot::channel();
        sender
            .try_send((json!({"op":"limit"}), Instant::now(), reply))
            .unwrap();
        let mut pause = enqueue(&sender, "pause");
        let mut limit = enqueue(&sender, "limit");
        let mut serviced = Vec::new();
        queue.service(|request| {
            serviced.push(request["op"].as_str().unwrap().to_string());
            Ok(json!({}))
        });
        assert_eq!(serviced, ["pause", "limit"]);
        assert!(pause.try_recv().unwrap().is_ok());
        assert!(limit.try_recv().unwrap().is_ok());
        assert!(queue.deferred.is_empty());
    }

    #[test]
    fn cooperative_deferral_has_a_hard_capacity() {
        let (sender, receiver) = mpsc::sync_channel(CONTROL_CAPACITY);
        let mut queue = ControlQueue::new(receiver);
        let mut replies = Vec::new();
        for _ in 0..CONTROL_CAPACITY {
            replies.push(enqueue(&sender, "move"));
        }
        for _ in 0..(CONTROL_CAPACITY / 16) {
            queue.service(|_| panic!("Storage mutation must remain queued"));
        }
        assert_eq!(queue.deferred.len(), CONTROL_CAPACITY);
        for _ in 0..CONTROL_CAPACITY {
            replies.push(enqueue(&sender, "move"));
        }
        queue.service(|_| panic!("Storage mutation must remain queued"));
        assert_eq!(queue.deferred.len(), CONTROL_CAPACITY);
        let (reply, _result) = oneshot::channel();
        assert!(matches!(
            sender.try_send((
                json!({"op":"move"}),
                Instant::now() + Duration::from_secs(1),
                reply
            )),
            Err(mpsc::TrySendError::Full(_))
        ));
        drop(replies);
        queue.service(|_| panic!("Cancelled storage mutation must not execute"));
        assert!(queue.deferred.is_empty());
    }

    #[test]
    fn metadata_progress_is_bounded_and_cancellation_is_observed() {
        let control = Arc::new(CreationControl::new(Duration::from_secs(1)));
        let ptr = Arc::as_ptr(&control).cast_mut().cast();
        assert!(creation_progress(ptr, 3, 10));
        assert!(creation_progress(ptr, 2, 10));
        assert_eq!(control.snapshot()["completedPieces"], 3);
        let cancel_on_drop = CancelCreationOnDrop(control.clone());
        drop(cancel_on_drop);
        assert!(!creation_progress(ptr, 4, 10));
        let expired = Arc::new(CreationControl::new(Duration::ZERO));
        assert!(!creation_progress(
            Arc::as_ptr(&expired).cast_mut().cast(),
            0,
            0
        ));
    }

    #[test]
    fn native_creation_rejects_cancellation_before_touching_output() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-create-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("preserved.torrent");
        std::fs::write(&output, b"previous metadata").unwrap();
        let control = Arc::new(CreationControl::new(Duration::ZERO));
        let result = invoke(
            json!({"op":"create", "path":root.join("missing.bin"), "output":output}),
            |input| unsafe {
                fr_torrent_create(
                    input,
                    creation_progress,
                    Arc::as_ptr(&control).cast_mut().cast(),
                )
            },
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(std::fs::read(&output).unwrap(), b"previous metadata");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_native_piece_hashing_preserves_previous_metadata() {
        extern "C" fn stop_after_first_piece(
            context: *mut c_void,
            completed: u64,
            _total: u64,
        ) -> bool {
            let observed = unsafe { &*context.cast::<AtomicU64>() };
            observed.fetch_max(completed, Ordering::Relaxed);
            completed == 0
        }
        let root = std::env::temp_dir().join(format!(
            "fetchrail-create-mid-cancel-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("payload.bin");
        let output = root.join("preserved.torrent");
        std::fs::write(&payload, vec![0x5a; 65536]).unwrap();
        std::fs::write(&output, b"previous metadata").unwrap();
        let observed = AtomicU64::new(0);
        let result = invoke(
            json!({"op":"create", "path":payload, "output":output, "format":"v1", "pieceLength":16384}),
            |input| unsafe {
                fr_torrent_create(
                    input,
                    stop_after_first_piece,
                    (&observed as *const AtomicU64).cast_mut().cast(),
                )
            },
        );
        assert!(result.unwrap_err().contains("cancelled"));
        assert!(observed.load(Ordering::Relaxed) > 0);
        assert_eq!(std::fs::read(&output).unwrap(), b"previous metadata");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_busy_metadata_worker_does_not_block_native_poll_or_remove() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-create-isolation-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("payload.bin");
        let output = root.join("created.torrent");
        std::fs::write(&payload, vec![0x5a; 32768]).unwrap();
        let engine = TorrentEngine::new(&root.join("state")).unwrap();
        let (entered, ready) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let mut control = CreationControl::new(Duration::from_secs(20));
        control.test_gate = Some(CreationTestGate {
            entered,
            release: Mutex::new(released),
            used: AtomicBool::new(false),
        });
        let control = Arc::new(control);
        let (reply, result) = oneshot::channel();
        engine.creation.sender.try_send(CreationRequest {
            request: json!({"op":"create", "path":payload, "output":output, "format":"v1", "pieceLength":16384}),
            control, reply,
        }).unwrap_or_else(|_| panic!("Creation fixture could not be queued"));
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        // The real native creation call is held at its first progress callback.
        // These controls must complete before that worker can be released.
        tokio::time::timeout(Duration::from_secs(5), engine.call(json!({"op":"poll"})))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            engine.call(json!({"op":"remove", "id":uuid::Uuid::new_v4()})),
        )
        .await
        .unwrap()
        .unwrap();
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), result)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(std::fs::metadata(&output).unwrap().len() > 0);
        engine.call(json!({"op":"clean"})).await.unwrap();
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[tokio::test]
    async fn startup_handshake_surfaces_native_init_failure_and_absent_removal_is_safe() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-native-init-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("unclean")).unwrap();
        assert!(TorrentEngine::new(&root).is_err());
        std::fs::remove_dir(root.join("unclean")).unwrap();
        let engine = TorrentEngine::new(&root).unwrap();
        engine
            .call(json!({"op":"remove", "id":uuid::Uuid::new_v4()}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        engine.call(json!({"op":"clean"})).await.unwrap();
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_checkpoint_failure_preserves_old_resume_then_explicit_retry_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!(
            "fetchrail-native-checkpoint-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("payload.bin");
        std::fs::write(&payload, vec![0x5a; 32768]).unwrap();
        let metadata = root.join("fixture.torrent");
        let engine = TorrentEngine::new(&root.join("state")).unwrap();
        engine.call(json!({"op":"create", "path":payload, "output":metadata, "format":"v1", "pieceLength":16384})).await.unwrap();
        let id = uuid::Uuid::new_v4();
        engine
            .call(
                json!({"op":"add", "id":id, "source":metadata, "destination":root,
            "config":{"dht":false,"lsd":false,"upnp":false,"listenInterfaces":"127.0.0.1:0"}}),
            )
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        let resume = root.join("state").join(format!("{id}.resume"));
        let previous = std::fs::read(&resume).unwrap();
        // Deny replacement of the known-good resume while the real native save runs.
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&resume)
            .unwrap();
        let error = engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap_err();
        assert!(error.contains("checkpoint"), "{error}");
        drop(locked);
        assert_eq!(std::fs::read(&resume).unwrap(), previous);
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"tracker", "id":id, "url":"http://127.0.0.1:1/announce"}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        let tracker = b"http://127.0.0.1:1/announce";
        assert!(std::fs::read(&resume)
            .unwrap()
            .windows(tracker.len())
            .any(|w| w == tracker));
        std::fs::remove_file(&resume).unwrap();
        engine.call(json!({"op":"checkpoint"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !resume.exists() {
                engine.call(json!({"op":"poll"})).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "A missing known-good checkpoint must be recreated, never accepted as not-modified",
        );
        let observation = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let poll = engine.call(json!({"op":"poll"})).await.unwrap();
                if let Some(state) = poll["states"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|state| state["id"] == id.to_string())
                {
                    break serde_json::from_value::<crate::health::TorrentHealth>(
                        state["health"].clone(),
                    )
                    .unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(observation.checkpoint_errors >= 1);
        let safe = serde_json::to_string(&observation).unwrap();
        assert!(!safe.contains("127.0.0.1") && !safe.contains("fixture.torrent"));
        engine.call(json!({"op":"remove", "id":id})).await.unwrap();
        engine.call(json!({"op":"clean"})).await.unwrap();
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
