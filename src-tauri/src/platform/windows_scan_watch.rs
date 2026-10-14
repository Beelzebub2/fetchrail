// Observe writes without preventing scanner remediation.
use std::{fs::File, os::windows::io::AsRawHandle};
use windows::{
    core::{HRESULT, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, ERROR_IO_PENDING, HANDLE, WAIT_TIMEOUT},
        System::{
            Ioctl::{
                FSCTL_REQUEST_OPLOCK, OPLOCK_LEVEL_CACHE_READ, REQUEST_OPLOCK_CURRENT_VERSION,
                REQUEST_OPLOCK_INPUT_BUFFER, REQUEST_OPLOCK_INPUT_FLAG_REQUEST,
                REQUEST_OPLOCK_OUTPUT_BUFFER,
            },
            Threading::{CreateEventW, WaitForSingleObject},
            IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED},
        },
    },
};

pub(crate) struct ScanWatch {
    file: File,
    event: HANDLE,
    overlapped: Box<OVERLAPPED>,
    output: Box<REQUEST_OPLOCK_OUTPUT_BUFFER>,
    pending: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
static HASH_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));
impl ScanWatch {
    pub(crate) fn new(file: File) -> Option<Self> {
        // Avoid adding an unbounded set of integrity readers to simultaneous scans.
        let permit = HASH_SLOTS.clone().try_acquire_owned().ok()?;
        let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?;
        let mut watch = Self {
            file,
            event,
            overlapped: Box::new(OVERLAPPED {
                hEvent: event,
                ..Default::default()
            }),
            output: Box::default(),
            pending: false,
            _permit: permit,
        };
        let input = REQUEST_OPLOCK_INPUT_BUFFER {
            StructureVersion: REQUEST_OPLOCK_CURRENT_VERSION as u16,
            StructureLength: std::mem::size_of::<REQUEST_OPLOCK_INPUT_BUFFER>() as u16,
            RequestedOplockLevel: OPLOCK_LEVEL_CACHE_READ,
            Flags: REQUEST_OPLOCK_INPUT_FLAG_REQUEST,
        };
        let result = unsafe {
            DeviceIoControl(
                HANDLE(watch.file.as_raw_handle()),
                FSCTL_REQUEST_OPLOCK,
                Some((&input as *const REQUEST_OPLOCK_INPUT_BUFFER).cast()),
                std::mem::size_of_val(&input) as u32,
                Some((&mut *watch.output as *mut REQUEST_OPLOCK_OUTPUT_BUFFER).cast()),
                std::mem::size_of_val(&*watch.output) as u32,
                None,
                Some(&mut *watch.overlapped),
            )
        };
        if result.err()?.code() != HRESULT::from_win32(ERROR_IO_PENDING.0) {
            return None;
        }
        watch.pending = true;
        Some(watch)
    }
    pub(crate) fn unchanged(&self) -> bool {
        unsafe { WaitForSingleObject(self.event, 0) == WAIT_TIMEOUT }
    }
}
impl Drop for ScanWatch {
    fn drop(&mut self) {
        unsafe {
            // Failed requests have no pending I/O to drain.
            if self.pending {
                let _ = CancelIoEx(HANDLE(self.file.as_raw_handle()), Some(&*self.overlapped));
                let mut bytes = 0;
                let _ = GetOverlappedResult(
                    HANDLE(self.file.as_raw_handle()),
                    &*self.overlapped,
                    &mut bytes,
                    true,
                );
            }
            let _ = CloseHandle(self.event);
        }
    }
}
