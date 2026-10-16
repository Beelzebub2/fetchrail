use serde_json::{json, Value};
use std::{
    fmt,
    sync::{Mutex, OnceLock},
    time::Instant,
};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Metadata, Subscriber,
};

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static INSTALLED: OnceLock<bool> = OnceLock::new();

struct State {
    started: Instant,
    utc: chrono::DateTime<chrono::Utc>,
    bdp: Vec<Value>,
    // second, connection update count/bytes, stream update count/bytes.
    windows: Vec<[u64; 5]>,
    truncated: bool,
}

impl State {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            utc: chrono::Utc::now(),
            bdp: Vec::new(),
            windows: Vec::new(),
            truncated: false,
        }
    }

    fn record(&mut self, ms: u64, event: Control) {
        if let Control::Window { stream, increment } = event {
            let second = ms / 1000;
            if self.windows.last().is_none_or(|row| row[0] != second) {
                if self.windows.len() == 600 {
                    self.truncated = true;
                    return;
                }
                self.windows.push([second, 0, 0, 0, 0]);
            }
            let row = self.windows.last_mut().unwrap();
            let index = if stream == 0 { 1 } else { 3 };
            row[index] += 1;
            row[index + 1] += u64::from(increment);
            return;
        }
        if self.bdp.len() == 256 {
            self.truncated = true;
            return;
        }
        self.bdp.push(match event {
            Control::Ack { bytes, rtt_ns } => {
                json!({"elapsedMs":ms,"kind":"ack","bytes":bytes,"rttNs":rtt_ns})
            }
            Control::Bandwidth(bytes) => {
                json!({"elapsedMs":ms,"kind":"bandwidth","bytesPerSecond":bytes})
            }
            Control::Target(bytes) => json!({"elapsedMs":ms,"kind":"target","bytes":bytes}),
            Control::Ping => json!({"elapsedMs":ms,"kind":"ping"}),
            Control::Pong => json!({"elapsedMs":ms,"kind":"pong"}),
            Control::Window { .. } => unreachable!(),
        });
    }
}

#[derive(Debug, PartialEq)]
enum Control {
    Window { stream: u32, increment: u32 },
    Ack { bytes: u64, rtt_ns: u64 },
    Bandwidth(u64),
    Target(u64),
    Ping,
    Pong,
}

fn decimal(value: &str, scale: f64) -> Option<u64> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let number = value.parse::<f64>().ok()? * scale;
    (number.is_finite() && number >= 0.0 && number < u64::MAX as f64).then_some(number as u64)
}

fn parse_message(message: &str) -> Option<Control> {
    if message.len() > 160 {
        return None;
    }
    if message == "sent ping" {
        return Some(Control::Ping);
    }
    if message == "recv pong" {
        return Some(Control::Pong);
    }
    if let Some(value) = message.strip_prefix("BDP increased to ") {
        return Some(Control::Target(value.parse().ok()?));
    }
    if let Some(value) = message
        .strip_prefix("current bandwidth = ")
        .and_then(|v| v.strip_suffix("B/s"))
    {
        return decimal(value, 1.0).map(Control::Bandwidth);
    }
    let (bytes, duration) = message
        .strip_prefix("received BDP ack; bytes = ")?
        .split_once(", rtt = ")?;
    for (unit, scale) in [
        ("ns", 1.0),
        ("µs", 1000.0),
        ("ms", 1_000_000.0),
        ("s", 1_000_000_000.0),
    ] {
        if let Some(value) = duration.strip_suffix(unit) {
            return Some(Control::Ack {
                bytes: bytes.parse().ok()?,
                rtt_ns: decimal(value, scale)?,
            });
        }
    }
    None
}

fn parse_frame(frame: &str) -> Option<Control> {
    let (stream, increment) = frame
        .strip_prefix("WindowUpdate { stream_id: StreamId(")?
        .split_once("), size_increment: ")?;
    let stream = stream.parse::<u32>().ok()?;
    let increment = increment.strip_suffix(" }")?.parse::<u32>().ok()?;
    (stream <= 0x7fff_ffff && (1..=0x7fff_ffff).contains(&increment))
        .then_some(Control::Window { stream, increment })
}

struct Fields(Option<Control>);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // Never retain arbitrary messages, request headers, URLs or frame data.
        self.0 = match field.name() {
            "message" => parse_message(&format!("{value:?}")).or(self.0.take()),
            "frame" => parse_frame(&format!("{value:?}")).or(self.0.take()),
            _ => self.0.take(),
        };
    }
}

struct Controls;
impl Subscriber for Controls {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event()
            && matches!(
                metadata.target(),
                "hyper::proto::h2::ping" | "h2::codec::framed_write"
            )
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields(None);
        event.record(&mut fields);
        if let (Some(control), Some(state)) = (fields.0, STATE.get()) {
            if let Ok(mut state) = state.lock() {
                let ms = state.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                state.record(ms, control);
            }
        }
    }
}

pub(crate) fn enable() {
    if std::env::var("FETCHRAIL_TEST_H2_FLOW").is_ok_and(|v| v == "1")
        && crate::platform::test_root().is_ok_and(|root| root.is_some())
    {
        STATE.get_or_init(|| Mutex::new(State::new()));
        INSTALLED.get_or_init(|| tracing::subscriber::set_global_default(Controls).is_ok());
    }
}

pub(super) fn snapshot() -> Value {
    let Some(state) = STATE.get().and_then(|state| state.lock().ok()) else {
        return Value::Null;
    };
    json!({"enabled":INSTALLED.get().copied().unwrap_or(false),"startedAtUtc":state.utc,"bdpEvents":state.bdp,"windowBuckets":state.windows,"truncated":state.truncated})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscriber_reads_real_events_and_discards_headers() {
        STATE.get_or_init(|| Mutex::new(State::new()));
        let dispatch = tracing::Dispatch::new(Controls);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::trace!(target: "hyper::proto::h2::ping", "BDP increased to {}", 65535);
            tracing::debug!(target: "h2::codec::framed_write", frame = ?format_args!("WindowUpdate {{ stream_id: StreamId(0), size_increment: 32768 }}"), "send");
            tracing::debug!(target: "h2::codec::framed_write", frame = ?format_args!("Headers {{ authorization: secret, url: https://private.invalid }}"), "send");
        });
        let state = STATE.get().unwrap().lock().unwrap();
        assert!(state
            .bdp
            .iter()
            .any(|event| event["kind"] == "target" && event["bytes"] == 65535));
        assert_eq!(state.windows.iter().map(|row| row[2]).sum::<u64>(), 32768);
        let saved = json!({"bdp":state.bdp,"windows":state.windows}).to_string();
        assert!(!saved.contains("private.invalid"));
        assert!(!saved.contains("secret"));
    }
    #[test]
    fn controls_accept_numeric_fields_and_reject_private_headers_and_messages() {
        assert_eq!(
            parse_message("received BDP ack; bytes = 65535, rtt = 2.5ms"),
            Some(Control::Ack {
                bytes: 65535,
                rtt_ns: 2_500_000
            })
        );
        assert_eq!(
            parse_message("BDP increased to 1048576"),
            Some(Control::Target(1048576))
        );
        assert_eq!(
            parse_message("current bandwidth = 12345.6B/s"),
            Some(Control::Bandwidth(12345))
        );
        assert_eq!(
            parse_frame("WindowUpdate { stream_id: StreamId(0), size_increment: 65535 }"),
            Some(Control::Window {
                stream: 0,
                increment: 65535
            })
        );
        for private in [
            "https://private.invalid/?token=secret",
            "Headers { authorization: secret }",
            "BDP increased to https://private.invalid",
            "current bandwidth = NaNB/s",
        ] {
            assert!(parse_message(private).is_none());
            assert!(parse_frame(private).is_none());
        }
        assert!(
            parse_frame("WindowUpdate { stream_id: StreamId(1), size_increment: 0 }").is_none()
        );
    }
    #[test]
    fn updates_aggregate_by_second_and_storage_is_bounded() {
        let mut state = State::new();
        state.record(
            1,
            Control::Window {
                stream: 0,
                increment: 64,
            },
        );
        state.record(
            2,
            Control::Window {
                stream: 3,
                increment: 32,
            },
        );
        state.record(
            3,
            Control::Window {
                stream: 0,
                increment: 64,
            },
        );
        assert_eq!(state.windows, vec![[0, 2, 128, 1, 32]]);
        for second in 1..1000 {
            state.record(
                second * 1000,
                Control::Window {
                    stream: 0,
                    increment: 1,
                },
            );
        }
        for ms in 0..1000 {
            state.record(ms, Control::Ping);
        }
        assert_eq!(state.windows.len(), 600);
        assert_eq!(state.bdp.len(), 256);
        assert!(state.truncated);
    }
}
