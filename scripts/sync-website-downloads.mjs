import { copyFile } from "node:fs/promises";
await copyFile(new URL("./release-downloads.mjs", import.meta.url), new URL("../website/public/downloads.mjs", import.meta.url));
console.log("Copied the tested release selector into the separately hosted website.");
