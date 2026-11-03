// The desktop application and Android app use the same origin admission,
// throttling and slow-link detection implementation. Do not fork these rules.
#[path = "../../../src-tauri/src/network.rs"]
mod network;
#[path = "../../../src-tauri/src/rate_limit.rs"]
mod rate_limit;

mod transfer;
pub use transfer::{enqueue, list, pause, remove, resume, run, stop};

#[cfg(target_os = "android")]
mod android_jni;
