// Explicit rollback export; never writes plaintext outside the existing private state folder.
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { parseStateSnapshot } from "./state-snapshot.mjs";

const [input, output] = process.argv.slice(2);
if (!input || !output) throw new Error("Usage: node scripts/export-state-snapshot.mjs INPUT.json OUTPUT.rollback.json (quit Fetchrail first)");
const source = resolve(input), destination = resolve(output);
const sameDirectory = process.platform === "win32" ? dirname(source).toLowerCase() === dirname(destination).toLowerCase() : dirname(source) === dirname(destination);
if (!sameDirectory || source === destination) throw new Error("Export to a new file in the same private state folder");
const value = parseStateSnapshot(await readFile(source));
await writeFile(destination, JSON.stringify(value, null, 2) + "\n", { flag: "wx", mode: 0o600 });
console.log("Private rollback snapshot exported. Keep it private; it can contain source URLs and credentials.");
