import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

// Run the real session queue and native protocol tests without building libtorrent.
const root = fileURLToPath(new URL("../", import.meta.url));
const out = join(root, "artifacts", "browser-session-native");
await mkdir(join(out, "src"), { recursive: true });
const model = await readFile(join(root, "src-tauri/src/model.rs"), "utf8");
const start = model.lastIndexOf("#[derive", model.indexOf("pub struct AddDownloadRequest"));
const end = model.lastIndexOf("#[derive", model.indexOf("pub struct QueueRecord"));
assert.ok(start >= 0 && end > start);
const path = (name) => JSON.stringify(join(root, "src-tauri/src", name).replaceAll("\\", "/"));
await writeFile(join(out, "src/lib.rs"), `#![allow(dead_code)]
mod model { use chrono::{DateTime,Utc}; use serde::{Serialize,Deserialize}; ${model.slice(start, end)} }
#[path = ${path("browser_session.rs")}] mod browser_session;
#[path = ${path("native_protocol.rs")}] mod native_protocol;
`);
await writeFile(join(out, "Cargo.toml"), `[package]
name = "fetchrail-browser-session-check"
version = "0.0.0"
edition = "2021"
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["serde", "v4"] }
url = "2"
`);
const child = spawn("cargo", ["test", "--manifest-path", join(out, "Cargo.toml"), "--offline"], { cwd: root, stdio: "inherit", windowsHide: true });
child.on("error", (error) => { console.error(error.message); process.exitCode = 1; });
child.on("exit", (code) => { process.exitCode = code ?? 1; });
