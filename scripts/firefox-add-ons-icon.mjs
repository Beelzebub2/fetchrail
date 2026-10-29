import { readFile } from "node:fs/promises";
import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { amoRequest } from "./firefox-add-ons-policy.mjs";

// AMO listing icons are separate metadata: manifest.icons only covers the installed extension.
// https://mozilla.github.io/addons-server/topics/api/addons.html#addon-icon
export async function syncIcon({ issuer, secret, guid, icon, fetcher = fetch, pause = setTimeout }) {
  if (!guid || !icon?.length) throw new Error("Firefox ID and listing icon are required.");
  const url = `https://addons.mozilla.org/api/v5/addons/addon/${encodeURIComponent(guid)}/`;
  const form = new FormData();
  form.set("icon", new Blob([icon], { type: "image/png" }), "fetchrail.png");
  const response = await amoRequest({ issuer, secret, fetcher, pause }, url, { method: "PATCH", body: form });
  if (!response.ok) throw new Error(`Mozilla AMO icon upload failed (HTTP ${response.status}).`);
  // Resizing is asynchronous. Wait for a custom 128px image to be available publicly.
  for (let attempt = 0; attempt < 12; attempt++) {
    if (attempt) await pause(5000);
    const listing = await amoRequest({ issuer, secret, fetcher, pause }, url, { headers: { "Cache-Control": "no-cache" } });
    if (!listing.ok) throw new Error(`Mozilla AMO icon verification failed (HTTP ${listing.status}).`);
    const iconUrl = (await listing.json()).icons?.["128"];
    if (typeof iconUrl !== "string" || iconUrl.includes("/addon-icons/default-")) continue;
    const image = await fetcher(iconUrl, { signal: AbortSignal.timeout(30_000) });
    if (image.ok && image.headers.get("content-type")?.startsWith("image/")) {
      console.log("Mozilla AMO listing icon uploaded; custom public icon is available.");
      return;
    }
  }
  throw new Error("Mozilla accepted the listing icon but its public image is not ready. Rerun policy_only to verify/repair metadata.");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const manifest = JSON.parse(await readFile(new URL("../browser-extension/manifests/firefox.json", import.meta.url), "utf8"));
  await syncIcon({
    issuer: process.env.AMO_JWT_ISSUER, secret: process.env.AMO_JWT_SECRET,
    guid: manifest.browser_specific_settings.gecko.id,
    icon: await readFile(new URL(`../browser-extension/src/${manifest.icons["128"]}`, import.meta.url)),
  });
}
