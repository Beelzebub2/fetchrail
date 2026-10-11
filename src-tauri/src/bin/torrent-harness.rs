//! Isolated native engine fixture. Uses only its caller-provided directory and
//! never touches the installed application's settings or torrent catalog.
use fetchrail_lib::torrent::TorrentEngine;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
#[tokio::main]
async fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("Pass an isolated state directory");
    let engine = TorrentEngine::new(std::path::Path::new(&root)).unwrap();
    for line in io::stdin().lock().lines() {
        let result = match line {
            Ok(line) => match serde_json::from_str::<Value>(&line) {
                Ok(request) => engine.call(request).await,
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        };
        let response = match result {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error}),
        };
        println!("{response}");
        io::stdout().flush().unwrap();
    }
}
