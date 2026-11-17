import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { browserFixture } from "./fixtures/extension-browser.mjs";

const fileUrl = "https://chatgpt.com/backend-api/estuary/content?fn=generated-report.zip";
const blobUrl = "blob:https://chatgpt.com/3f57eaf6-38a6-4f74-9d3f-9aa32876ab01";

function setBrowserBlob(f, overrides = {}) {
  Object.assign(f.item, {
    url: blobUrl, finalUrl: blobUrl, referrer: "https://chatgpt.com/c/test-chat",
    tabId: 1, filename: "C:\\Downloads\\generated-report.zip", ...overrides,
  });
}
async function recordXHR(f, overrides = {}) {
  const { url = fileUrl, requestId = "chatgpt-content", tabId = 1, statusCode = 200,
    method = "GET", type = "xmlhttprequest", filename = "generated-report.zip", disposition = true, ...extra } = overrides;
  const details = { url, requestId, tabId, incognito: !!f.item.incognito, method, type,
    cookieStoreId: f.item.cookieStoreId, ...extra };
  f.events.get("before")(details);
  f.events.get("headers")({ ...details, requestHeaders: [{ name: "Cookie", value: "chatgpt-test-session" }] });
  f.events.get("response")({ ...details, statusCode, responseHeaders: [
    ...(disposition ? [{ name: "Content-Disposition", value: `attachment; filename="${filename}"` }] : []),
    { name: "Content-Type", value: "application/zip" },
  ] });
  f.events.get("completed")({ ...details, statusCode });
  await new Promise(setImmediate);
}
async function beginBlob(f) {
  f.create();
  let released = 0;
  if (f.events.has("filename")) {
    assert.equal(f.events.get("filename")({ ...f.item }, () => released++), true);
  }
  await f.settle();
  if (f.events.has("filename")) assert.equal(released, 1, "Every Chromium filename decision must be released exactly once");
}
async function unsupportedBlob(f) {
  f.create();
  if (f.events.has("filename")) {
    let released = 0;
    const decision = f.events.get("filename")({ ...f.item }, () => released++);
    if (decision === true) {
      // The background worker is async; allow it to register the evidence
      // timeout before asking the fake clock to fire the pending callback.
      await new Promise(setImmediate);
      await f.tick();
    }
    await f.settle();
    assert.equal(released, 1, "An unsupported blob must keep the browser filename");
  } else {
    await new Promise(setImmediate);
    await f.tick();
  }
  assert.equal(f.stages.size, 0);
  assert.equal(f.actions.includes("cancel"), false, "Unmatched content must not be cancelled");
  assert.equal(f.actions.includes("erase"), false, "Unmatched content must not lose browser history");
}

for (const family of ["chromium", "firefox"]) {
  const ok = await browserFixture(family);
  setBrowserBlob(ok);
  await recordXHR(ok);
  await beginBlob(ok);
  assert.deepEqual(ok.actions, family === "chromium" ? ["cancel", "erase"] : ["pause", "cancel", "erase"]);
  assert.equal(ok.stages.size, 1);
  assert.equal([...ok.stages.values()][0].committed, true);
  const capture = ok.calls.find(call => call.method === "addDownloads")?.params.items[0];
  assert.equal(capture.url, fileUrl, "Fetchrail must receive the original HTTPS file request, never a blob URL");
  assert.equal(capture.suggestedFileName, "generated-report.zip");
  assert.equal(capture.requestContext, undefined, "Session headers stay opt-in");
  assert.ok(!JSON.stringify(ok.session).includes("/backend-api/estuary/content"), "Signed file URLs must not persist without session opt-in");

  const queryOnly = await browserFixture(family);
  setBrowserBlob(queryOnly);
  await recordXHR(queryOnly, { disposition: false });
  await beginBlob(queryOnly);
  assert.equal(queryOnly.stages.size, 1, "The ChatGPT fn query parameter must identify the file without a Content-Disposition header");

  const interpreter = await browserFixture(family);
  setBrowserBlob(interpreter);
  await recordXHR(interpreter, { url: "https://chatgpt.com/backend-api/conversation/example/interpreter/download?download_intent=true" });
  await beginBlob(interpreter);
  assert.equal(interpreter.stages.size, 1, "Interpreter output can use the Content-Disposition filename without fn");

  const signedIn = await browserFixture(family);
  signedIn.saved.useBrowserSession = true;
  await signedIn.boot();
  setBrowserBlob(signedIn);
  await recordXHR(signedIn);
  await beginBlob(signedIn);
  assert.equal(signedIn.calls.find(call => call.method === "addDownloads").params.items[0].requestContext.cookie, "chatgpt-test-session");
  assert.ok(!JSON.stringify(signedIn.saved).includes("chatgpt-test-session"), "Sensitive headers must not reach durable storage");

  const redirect = await browserFixture(family);
  setBrowserBlob(redirect);
  const source = { url: fileUrl, requestId: "redirect", tabId: 1, incognito: false, method: "GET", type: "xmlhttprequest" };
  redirect.events.get("headers")({ ...source, requestHeaders: [{ name: "Authorization", value: "Bearer original-site-only" }] });
  const finalUrl = "https://files.oaiusercontent.com/signed-file";
  redirect.events.get("headers")({ ...source, url: finalUrl, requestHeaders: [] });
  redirect.events.get("response")({ ...source, url: finalUrl, statusCode: 200,
    responseHeaders: [{ name: "Content-Disposition", value: 'attachment; filename="generated-report.zip"' }] });
  redirect.events.get("completed")({ ...source, url: finalUrl, statusCode: 200 });
  await new Promise(setImmediate);
  await beginBlob(redirect);
  const redirected = redirect.calls.find(call => call.method === "addDownloads").params.items[0];
  assert.equal(redirected.url, finalUrl, "Replayed redirects must use the final observed HTTPS URL");
  assert.equal(redirected.requestContext, undefined, "Credentials must never leak to a redirect origin");

  const completed = await browserFixture(family);
  setBrowserBlob(completed);
  await recordXHR(completed);
  completed.faults.afterStage = item => { item.state = "complete"; item.paused = false; };
  await beginBlob(completed);
  assert.equal([...completed.stages.values()][0].state, "aborted");
  assert.equal(completed.actions.includes("cancel"), false, "A completed browser file must never be cancelled");

  for (const [label, xhr, browser] of [
    ["mismatched filename", {}, { filename: "C:\\Downloads\\unrelated.zip" }],
    ["wrong tab", { tabId: 2 }, {}],
    ["non-file XHR", { url: "https://chatgpt.com/backend-api/conversation" }, {}],
    ["POST result", { method: "POST" }, {}],
    ["failed request", { statusCode: 404 }, {}],
  ]) {
    const f = await browserFixture(family);
    setBrowserBlob(f, browser);
    await recordXHR(f, xhr);
    await unsupportedBlob(f);
    assert.equal(f.item.state, "in_progress", label + " must preserve browser ownership");
  }
  const unrelated = await browserFixture(family);
  setBrowserBlob(unrelated, { url: "blob:https://example.test/id", finalUrl: "blob:https://example.test/id" });
  await recordXHR(unrelated);
  unrelated.create();
  if (unrelated.events.has("filename")) unrelated.events.get("filename")({ ...unrelated.item }, () => {});
  await unrelated.settle();
  assert.deepEqual(unrelated.actions, [], "Other websites' blobs must never be intercepted");

  const ambiguous = await browserFixture(family);
  setBrowserBlob(ambiguous);
  await recordXHR(ambiguous, { requestId: "first" });
  await recordXHR(ambiguous, { requestId: "second" });
  await unsupportedBlob(ambiguous);

  for (const [reason, preferences] of [
    ["automatic capture disabled", { automaticDownloads: false }],
    ["excluded website", { captureRules: { excludedHosts: ["chatgpt.com"] } }],
    ["excluded file type", { captureRules: { excludedExtensions: ["zip"] } }],
  ]) {
    const optedOut = await browserFixture(family);
    Object.assign(optedOut.saved, preferences);
    setBrowserBlob(optedOut);
    optedOut.create();
    if (optedOut.events.has("filename")) {
      let released = 0;
      optedOut.events.get("filename")({ ...optedOut.item }, () => released++);
      await optedOut.settle();
      assert.equal(released, 1, reason + " must release the normal browser file");
    } else await optedOut.settle();
    assert.deepEqual(optedOut.actions, [], reason + " must not alter the browser download");
    assert.equal(optedOut.saved.recentOutcomes?.length ?? 0, 0, reason + " must not produce a capture failure warning");
  }

  console.log(`PASS ${family}: exact ChatGPT XHR-to-blob matching, session isolation, redirects, and browser fallback`);
}
for (const family of ["chromium", "firefox"]) {
  const manifest = JSON.parse(await readFile(`browser-extension/manifests/${family}.json`, "utf8"));
  assert.deepEqual(manifest.host_permissions, ["https://chatgpt.com/*"], "Only the ChatGPT origin should gain automatic request observation");
}
