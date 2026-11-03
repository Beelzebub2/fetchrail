use std::path::Path;

use jni::{
    objects::{JClass, JString},
    sys::jstring,
    JNIEnv,
};
use serde_json::json;

use crate::transfer;

fn arg(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
    env.get_string(value)
        .map(|v| v.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

fn reply(env: &mut JNIEnv<'_>, result: Result<serde_json::Value, String>) -> jstring {
    let body = match result {
        Ok(value) => json!({ "ok": true, "value": value }),
        Err(error) => json!({ "ok": false, "error": error }),
    };
    env.new_string(body.to_string())
        .map(|string| string.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_rrmtools_fetchrail_NativeBridge_enqueue(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    root: JString<'_>,
    url: JString<'_>,
) -> jstring {
    let result = (|| {
        let root = arg(&mut env, &root)?;
        let url = arg(&mut env, &url)?;
        transfer::enqueue(Path::new(&root), &url).map(|id| json!(id))
    })();
    reply(&mut env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_rrmtools_fetchrail_NativeBridge_list(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    root: JString<'_>,
) -> jstring {
    let result = (|| {
        let root = arg(&mut env, &root)?;
        let value = transfer::list(Path::new(&root))?;
        serde_json::from_str(&value).map_err(|e| e.to_string())
    })();
    reply(&mut env, result)
}

macro_rules! operation {
    ($jni_name:ident, $operation:ident) => {
        #[no_mangle]
        pub extern "system" fn $jni_name(
            mut env: JNIEnv<'_>,
            _: JClass<'_>,
            root: JString<'_>,
            id: JString<'_>,
        ) -> jstring {
            let result = (|| {
                let root = arg(&mut env, &root)?;
                let id = arg(&mut env, &id)?;
                transfer::$operation(Path::new(&root), &id).map(|_| json!(null))
            })();
            reply(&mut env, result)
        }
    };
}

operation!(Java_com_rrmtools_fetchrail_NativeBridge_run, run);
operation!(Java_com_rrmtools_fetchrail_NativeBridge_stop, stop);
operation!(Java_com_rrmtools_fetchrail_NativeBridge_pause, pause);
operation!(Java_com_rrmtools_fetchrail_NativeBridge_resume, resume);
operation!(Java_com_rrmtools_fetchrail_NativeBridge_remove, remove);
