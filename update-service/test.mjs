import assert from "node:assert/strict";
import { AUDIENCE, REPOSITORY, FeedError, authorize, compareTags, loadRelease, publicationDecision, readJson, sha256, validateRelease } from "./policy.mjs";
import { notifyRelease } from "../scripts/notify-update-feed.mjs";

export async function tokenFixture(tag = "v1.2.3") {
  const pair = await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048,
    publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]);
  const jwk = { ...await crypto.subtle.exportKey("jwk", pair.publicKey), kid: "fixture", alg: "RS256", use: "sig" };
  const now = Math.floor(Date.now() / 1000);
  const claims = { iss: "https://token.actions.githubusercontent.com", aud: AUDIENCE, repository: REPOSITORY,
    repository_id: "1411136515", repository_owner_id: "65181309", ref: `refs/tags/${tag}`,
    workflow_ref: `${REPOSITORY}/.github/workflows/release.yml@refs/tags/${tag}`, event_name: "push", iat: now, nbf: now - 5, exp: now + 300 };
  const encoded = value => Buffer.from(JSON.stringify(value)).toString("base64url");
  async function sign(changes = {}, header = {}) {
    const body = `${encoded({ alg: "RS256", kid: "fixture", ...header })}.${encoded({ ...claims, ...changes })}`;
    const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, new TextEncoder().encode(body));
    return `${body}.${Buffer.from(signature).toString("base64url")}`;
  }
  return { sign, jwk, claims, now };
}
export function releaseFixture(tag = "v1.2.3") {
  const version = tag.slice(1), base = `https://github.com/${REPOSITORY}/releases/download/${tag}/`;
  const names = ["latest.json", `Fetchrail-v${version}-windows-x64.exe`, `Fetchrail-Setup-${tag}-windows-x64.exe`];
  for (const arch of ["x64", "arm64"]) for (const format of ["deb", "rpm", "AppImage", "tar.gz"])
    names.push(`Fetchrail-${tag}-linux-${arch}.${format}`);
  const release = { tag_name: tag, draft: false, prerelease: false, published_at: "2026-10-10T00:00:00Z",
    assets: names.map(name => ({ name, size: 1234, browser_download_url: base + name })) };
  const platforms = {};
  for (const [key, name] of [["windows-x86_64", names[1]], ["linux-x86_64-appimage", `Fetchrail-${tag}-linux-x64.AppImage`],
    ["linux-aarch64-appimage", `Fetchrail-${tag}-linux-arm64.AppImage`]])
    platforms[key] = { url: base + name, signature: Buffer.from("synthetic signature; never installed").toString("base64") };
  const manifest = { version, pub_date: release.published_at, platforms };
  return { release, manifest, bytes: new TextEncoder().encode(JSON.stringify(manifest)) };
}

if (process.argv[1]?.endsWith("test.mjs")) {
  const { sign, jwk, now, claims } = await tokenFixture();
  const jwks = async url => {
    assert.equal(url, "https://token.actions.githubusercontent.com/.well-known/jwks");
    return Response.json({ keys: [jwk] });
  };
  await authorize(await sign({}, { jku: "https://attacker.invalid/keys" }), "v1.2.3", jwks, now);
  for (const changes of [{ repository: "attacker/fetchrail" }, { repository_id: "1" }, { repository_owner_id: "1" },
    { aud: "https://attacker.invalid" }, { iss: "https://attacker.invalid" }, { exp: now }, { iat: now + 100 }, { nbf: now + 100 },
    { exp: now + 3600 }, { event_name: "pull_request" }, { ref: "refs/heads/main" }, { workflow_ref: `${REPOSITORY}/.github/workflows/other.yml@refs/tags/v1.2.3` }])
    await assert.rejects(authorize(await sign(changes), "v1.2.3", jwks, now), error => error.status === 401);
  await authorize(await sign({ ref: "refs/heads/main", event_name: "workflow_dispatch",
    workflow_ref: `${REPOSITORY}/.github/workflows/update-feed.yml@refs/heads/main` }), "v1.2.3", jwks, now);
  await assert.rejects(authorize(await sign({}, { alg: "HS256" }), "v1.2.3", jwks, now), error => error.status === 401);
  await assert.rejects(authorize(await sign({}, { kid: "unknown" }), "v1.2.3", jwks, now), error => error.status === 401);
  let keyReads = 0;
  await authorize(await sign(), "v1.2.3", async (_, options) => {
    assert.equal(options.cf.cacheTtl, keyReads++ === 0 ? 300 : 0);
    return Response.json({ keys: keyReads === 1 ? [] : [jwk] });
  }, now);
  await assert.rejects(authorize(await sign(), "v1.2.3", async () => { throw Error("offline"); }, now), error => error.status === 502);
  const token = await sign();
  await assert.rejects(authorize(token.slice(0, -8) + "AAAAAAAA", "v1.2.3", jwks, now), error => error.status === 401);
  await assert.rejects(authorize(token, "v1.2.4", jwks, now), error => error.status === 401);
  await assert.rejects(authorize("junk", "v1.2.3", jwks, now), error => error.status === 401);
  await assert.rejects(readJson(new Response("x".repeat(2048)), 1024), error => error.status === 413);
  await assert.rejects(readJson(new Response("invalid")), error => error.status === 400);
  let cancelled = false;
  const stalled = new ReadableStream({ cancel() { cancelled = true; } });
  await assert.rejects(readJson(new Response(stalled), 1024, 20), error => error.status === 408);
  assert.equal(cancelled, true, "A stalled metadata/request body is cancelled within its time limit");
  assert.equal(compareTags("v1.10.0", "v1.9.99"), 1);
  assert.throws(() => compareTags("v1.2.3-beta", "v1.2.3"), FeedError);

  const fixture = releaseFixture();
  const digest = await sha256(fixture.bytes);
  const upstream = async url => {
    if (url === `https://api.github.com/repos/${REPOSITORY}/releases/tags/v1.2.3`) return Response.json(fixture.release);
    assert.equal(url, `https://github.com/${REPOSITORY}/releases/download/v1.2.3/latest.json`);
    return new Response(fixture.bytes);
  };
  const snapshot = await loadRelease("v1.2.3", digest, upstream);
  await assert.rejects(loadRelease("v1.2.3", "0".repeat(64), upstream), error => error.status === 409);
  for (const property of ["draft", "prerelease"])
    assert.throws(() => validateRelease({ ...fixture.release, [property]: true }, fixture.manifest, "v1.2.3"), FeedError);
  assert.throws(() => validateRelease(fixture.release, { ...fixture.manifest, version: "1.2.4" }, "v1.2.3"), FeedError);
  const bad = structuredClone(fixture.manifest);
  bad.platforms["windows-x86_64"].url = "https://attacker.invalid/update.exe";
  assert.throws(() => validateRelease(fixture.release, bad, "v1.2.3"), FeedError);
  delete bad.platforms["windows-x86_64"];
  assert.throws(() => validateRelease(fixture.release, bad, "v1.2.3"), FeedError);
  assert.equal(publicationDecision(snapshot, snapshot), false);
  assert.throws(() => publicationDecision(snapshot, { ...snapshot, manifestSha256: "0".repeat(64) }), /immutable/);
  const newer = { ...snapshot, release: { ...snapshot.release, tag_name: "v1.2.4" } };
  assert.equal(publicationDecision(snapshot, newer), true);
  assert.throws(() => publicationDecision(newer, snapshot), /newer/);

  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: REPOSITORY,
    ACTIONS_ID_TOKEN_REQUEST_URL: "https://token-fixture.invalid/request?x=1", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "fixture-secret" };
  let posts = 0, sleeps = 0;
  const publisher = async (url, options) => {
    if (String(url).startsWith(env.ACTIONS_ID_TOKEN_REQUEST_URL)) {
      assert.equal(new URL(url).searchParams.get("audience"), AUDIENCE);
      return Response.json({ value: token });
    }
    assert.equal(url, AUDIENCE);
    assert.deepEqual(JSON.parse(options.body), { tag: "v1.2.3", manifestSha256: digest });
    assert.equal(options.headers.Authorization, `Bearer ${token}`);
    assert.equal(options.redirect, "error");
    return ++posts === 1 ? new Response(null, { status: 503 }) : Response.json({ version: "1.2.3", changed: true });
  };
  assert.deepEqual(await notifyRelease("v1.2.3", fixture.bytes, env, publisher, async () => { sleeps++; }), { version: "1.2.3", changed: true });
  assert.equal(posts, 2); assert.equal(sleeps, 1);
  await assert.rejects(notifyRelease("v1.2.3", fixture.bytes, {}, publisher), /trusted GitHub/);
  await assert.rejects(notifyRelease("v1.2.3", fixture.bytes, { ...env, FETCHRAIL_MANIFEST_SHA256: "0".repeat(64) }, publisher), /cryptographically verified/);
  console.log("PASS: signed GitHub OIDC scope/expiry, pinned keys, bounded metadata, public immutable assets, checksum binding, rollback protection and publisher retries.");
}
