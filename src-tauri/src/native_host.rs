use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    thread,
    time::Duration,
};

use crate::native_protocol::{
    validate_request, BrowserBridgeConfig, BrowserBridgeEnvelope, NativeMethod, NativeRequest,
    NativeResponse, MAX_NATIVE_REQUEST_BYTES, MAX_NATIVE_RESPONSE_BYTES,
};

pub const CHROMIUM_EXTENSION_ID: &str = "fkmedfamaoejlhddajndhjemiedmnldh";
pub const CHROMIUM_STORE_EXTENSION_ID: &str = "ccmbmcgihlemlheldpkgnidgohlkaipb";
pub const FIREFOX_EXTENSION_ID: &str = "browser@braid.rrmtools.uk";
pub const LOCAL_FIREFOX_EXTENSION_ID: &str = "{bb4d3986-35bd-4e55-bbcb-bb7f67894086}";

pub fn is_browser_invocation() -> bool {
    is_browser_invocation_args(std::env::args_os().skip(1))
}

fn is_browser_invocation_args(args: impl IntoIterator<Item = OsString>) -> bool {
    let chromium_origins = [
        format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/"),
        format!("chrome-extension://{CHROMIUM_STORE_EXTENSION_ID}/"),
    ];
    args.into_iter().any(|argument| {
        let argument = argument.to_string_lossy();
        chromium_origins
            .iter()
            .any(|origin| argument == origin.as_str())
            || argument == FIREFOX_EXTENSION_ID
            || argument == LOCAL_FIREFOX_EXTENSION_ID
    })
}

pub fn run() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    loop {
        let payload = match read_native_message(&mut input) {
            Ok(Some(payload)) => payload,
            Ok(None) => break,
            Err(message) => {
                let response = NativeResponse::failure("", "BAD_REQUEST", message);
                let _ = write_native_message(&mut output, &response);
                break;
            }
        };
        let response = handle_payload(&payload);
        if write_native_message(&mut output, &response).is_err() {
            break;
        }
    }
}

fn handle_payload(payload: &[u8]) -> NativeResponse {
    let request = match serde_json::from_slice::<NativeRequest>(payload) {
        Ok(request) => request,
        Err(error) => {
            return NativeResponse::failure(
                "",
                "BAD_REQUEST",
                format!("Invalid native message: {error}"),
            )
        }
    };
    if let Err(message) = validate_request(&request) {
        return NativeResponse::failure(request.id, "BAD_REQUEST", message);
    }
    let id = request.id.clone();
    match forward_request(request) {
        Ok(response) => response,
        Err(message) => NativeResponse::failure(id, "APP_UNAVAILABLE", message),
    }
}

fn forward_request(request: NativeRequest) -> Result<NativeResponse, String> {
    // WebView2 and companion deployment can take several seconds on a cold start.
    for attempt in 0..100 {
        if let Ok(config) = read_bridge_config() {
            match send_to_bridge(&config, &request) {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                // Reads can be retried after a reset; never replay an uncertain write.
                Err(_)
                    if matches!(
                        request.method,
                        NativeMethod::Ping | NativeMethod::GetDownloads
                    ) && attempt < 2 =>
                {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(message) => return Err(message),
            }
        }
        if attempt == 0 {
            launch_fetchrail()?;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("Fetchrail did not start its browser bridge in time.".into())
}

fn send_to_bridge(
    config: &BrowserBridgeConfig,
    request: &NativeRequest,
) -> Result<Option<NativeResponse>, String> {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", config.port)) else {
        return Ok(None);
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| format!("Could not configure Fetchrail connection: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("Could not configure Fetchrail connection: {error}"))?;
    let envelope = BrowserBridgeEnvelope {
        token: config.token.clone(),
        request: request.clone(),
    };
    let mut bytes = serde_json::to_vec(&envelope)
        .map_err(|error| format!("Could not encode Fetchrail browser request: {error}"))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|error| format!("Could not send request to Fetchrail: {error}"))?;

    let mut reader = BufReader::new(stream).take((MAX_NATIVE_RESPONSE_BYTES + 1) as u64);
    let mut response = Vec::new();
    reader
        .read_until(b'\n', &mut response)
        .map_err(|error| format!("Could not read Fetchrail response: {error}"))?;
    if response.len() > MAX_NATIVE_RESPONSE_BYTES {
        return Err("Fetchrail returned an oversized browser response.".into());
    }
    serde_json::from_slice(response.trim_ascii())
        .map(Some)
        .map_err(|error| format!("Fetchrail returned an invalid browser response: {error}"))
}

fn read_bridge_config() -> Result<BrowserBridgeConfig, String> {
    let path = crate::platform::app_data_dir()?.join("browser-bridge.json");
    let bytes =
        fs::read(path).map_err(|error| format!("Browser bridge state is unavailable: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Browser bridge state is invalid: {error}"))
}

/// The port of a running app's bridge, as last published.
#[cfg(windows)]
pub(crate) fn bridge_port() -> Option<u16> {
    read_bridge_config().ok().map(|config| config.port)
}

fn launch_fetchrail() -> Result<(), String> {
    crate::platform::launch(&["--background"])
}

fn read_native_message(reader: &mut impl Read) -> Result<Option<Vec<u8>>, String> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(format!("Could not read native message length: {error}")),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_NATIVE_REQUEST_BYTES {
        return Err("Native message size is invalid.".into());
    }
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("Could not read native message: {error}"))?;
    Ok(Some(payload))
}

fn write_native_message(writer: &mut impl Write, response: &NativeResponse) -> Result<(), String> {
    let payload = serde_json::to_vec(response)
        .map_err(|error| format!("Could not encode native response: {error}"))?;
    if payload.len() > MAX_NATIVE_RESPONSE_BYTES {
        return Err("Native response is too large.".into());
    }
    let length =
        u32::try_from(payload.len()).map_err(|_| "Native response is too large.".to_string())?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|_| writer.write_all(&payload))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("Could not write native response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_browser_launch_arguments() {
        assert!(is_browser_invocation_args([OsString::from(format!(
            "chrome-extension://{CHROMIUM_EXTENSION_ID}/"
        ))]));
        assert!(is_browser_invocation_args([OsString::from(format!(
            "chrome-extension://{CHROMIUM_STORE_EXTENSION_ID}/"
        ))]));
        assert!(is_browser_invocation_args([
            OsString::from(r"C:\manifest\com.rrmtools.braid.firefox.json"),
            OsString::from(FIREFOX_EXTENSION_ID),
        ]));
        assert!(!is_browser_invocation_args([OsString::from(
            "--background",
        )]));
        assert!(!is_browser_invocation_args([OsString::from(
            "chrome-extension://arbitrary-extension-id/",
        )]));
    }
}
