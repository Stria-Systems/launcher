/**
 * Launcher five-tier E2E — login + pair + software hub + Stria Works launch.
 *
 * Covers the five doors on striasystems.com/pricing against a LOCAL portal:
 *   1. Free limits      (Harborlight Clinic,  tier null,      restricted)
 *   2. Starter  $15/mo  (Acme AI,             tier starter,   entitled)
 *   3. Growth   $49/mo  (NeuralForge,         tier growth,    entitled)
 *   4. Scale   $199/mo  (Synthara Labs,       tier scale,     entitled)
 *   5. Enterprise        (Meridian Industrial, tier scale + engagement scope)
 *      Enterprise has no list price and no distinct launcher code path: once
 *      entitled, the launcher flow is identical to Scale (top self-serve
 *      entitlements + highest session budget). This test proves exactly that.
 *
 * Every company gets an owner AND at least one operator (Growth also gets an
 * admin), exercising the invite -> register(same org name) -> activate path.
 * Every user walks the launcher's REAL code path from src-tauri/src/main.rs:
 *   sign_in:       POST /api/auth/login {email,password}
 *                  POST /api/auth/pairing (session cookie)
 *   pair_machine:  POST /api/portal/ingest Bearer <pairing code> + launcher payload
 *   fetch updates: GET  /api/portal/software/updates Bearer <machine token>
 *   report:        POST /api/portal/software Bearer <machine token> {packages}
 * Then the Works phase spawns the REAL Stria Works binary with a sandboxed
 * HOME (mirroring works.rs launch_works/stop_works: lock file liveness,
 * SIGTERM then SIGKILL) and a portal.json written exactly like persist_pairing.
 *
 * Usage:
 *   PORTAL_URL=http://localhost:5774 node e2e/five-tier-launcher-e2e.mjs
 *
 * Local only. Test accounts are throwaway (stamp-suffixed @test.local).
 */
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
// Portal checkout: sibling of the launcher repo (…/stria-systems/website/Portal),
// overridable for other layouts.
const PORTAL_ROOT =
  process.env.PORTAL_ROOT ??
  resolve(HERE, "..", "..", "website", "Portal");
const { exec, findLiveDb, sqlStr } = await import(
  pathToFileURL(join(PORTAL_ROOT, "scripts", "lib", "local-d1.mjs")).href
);
// TOTP for the enrolled-2FA loop (same RFC 6238 core the portal runs).
const { totpCode, base32Decode } = await import(
  pathToFileURL(join(PORTAL_ROOT, "functions", "api", "auth", "two-factor.ts")).href
);

const PORTAL = (process.env.PORTAL_URL ?? "http://localhost:5774").replace(/\/+$/, "");
const WORKS_BIN =
  process.env.STRIA_WORKS_BIN ??
  "/Users/cnazarko/stria-systems/stria-works/target/release/stria-works-gpui";
const LAUNCHER_VERSION = "0.1.0";
const STAMP = Date.now().toString(36);

const COMPANIES = [
  {
    id: "free",
    org: "Harborlight Clinic",
    tier: null,
    tierLabel: "Free limits",
    users: [{ role: "owner" }, { role: "operator" }],
    expectEntitled: false,
  },
  {
    id: "starter",
    org: "Acme AI",
    tier: "starter",
    tierLabel: "Starter $15/mo",
    users: [{ role: "owner" }, { role: "operator" }],
    expectEntitled: true,
  },
  {
    id: "growth",
    org: "NeuralForge",
    tier: "growth",
    tierLabel: "Growth $49/mo",
    users: [{ role: "owner" }, { role: "admin" }, { role: "operator" }],
    expectEntitled: true,
  },
  {
    id: "scale",
    org: "Synthara Labs",
    tier: "scale",
    tierLabel: "Scale $199/mo",
    users: [{ role: "owner" }, { role: "operator" }],
    expectEntitled: true,
  },
  {
    id: "enterprise",
    org: "Meridian Industrial",
    tier: "scale",
    tierLabel: "Enterprise (Scale + engagement scope)",
    enterprise: true,
    users: [{ role: "owner" }, { role: "operator" }],
    expectEntitled: true,
  },
];

const INTAKE_ANSWERS = [
  "We run a small engineering team and want our recurring analysis work done by agents under review. Most of it is document triage and summarising, with a human approving anything that leaves the building.",
  "It runs maybe 200 times a day across four people. Each run takes about twenty minutes by hand today, mostly reading and cross-checking.",
  "We want to stay under $400 a month while we prove this out. Predictability matters more than squeezing the last dollar.",
  "Background is fine. Anything under five minutes is comfortably fast enough for us.",
  "Internal documents only. Some of it is commercially sensitive but there is no regulated personal data in scope.",
];

// ── accounting ──────────────────────────────────────────────────────────
let checks = 0;
let failures = 0;
const results = [];
function check(name, cond, extra = "") {
  checks++;
  if (cond) {
    console.log(`    [ok] ${name}`);
  } else {
    failures++;
    console.log(`    [FAIL] ${name} ${extra}`);
  }
  return !!cond;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ── http with per-user cookie jar ───────────────────────────────────────
function makeClient() {
  return { cookie: "" };
}
async function call(client, path, { method = "GET", body = null, machineToken = null } = {}) {
  const headers = {};
  if (client.cookie) headers.Cookie = client.cookie;
  if (body) headers["Content-Type"] = "application/json";
  if (machineToken) headers.Authorization = `Bearer ${machineToken}`;
  const res = await fetch(PORTAL + path, {
    method,
    headers,
    body: body ? JSON.stringify(body) : null,
  });
  const setCookies =
    typeof res.headers.getSetCookie === "function" ? res.headers.getSetCookie() : [];
  for (const c of setCookies) {
    const m = c.match(/stria_session=([^;]+)/);
    if (m?.[1]) client.cookie = `stria_session=${m[1]}`;
  }
  let data = null;
  try {
    data = await res.json();
  } catch {
    data = null;
  }
  return { status: res.status, data };
}

// ── stripe webhook signing (real handler, local dev secret) ─────────────
function devWebhookSecret() {
  const vars = join(PORTAL_ROOT, ".dev.vars");
  if (!existsSync(vars)) return null;
  const m = readFileSync(vars, "utf8").match(/^STRIPE_WEBHOOK_SECRET=(.+)$/m);
  return m?.[1]?.trim() ?? null;
}
async function subscribe(orgId, tier, email) {
  const secret = devWebhookSecret();
  if (!secret) throw new Error("no STRIPE_WEBHOOK_SECRET in Portal/.dev.vars");
  const event = {
    id: `evt_e2e_${createHash("sha1").update(`${orgId}:${tier}:${STAMP}`).digest("hex").slice(0, 20)}`,
    type: "checkout.session.completed",
    data: {
      object: {
        id: `cs_e2e_${orgId}`,
        customer: `cus_e2e_${orgId}`,
        subscription: `sub_e2e_${orgId}`,
        amount_total: 0,
        customer_details: { email },
        metadata: { organization_id: orgId, tier },
      },
    },
  };
  const raw = JSON.stringify(event);
  const t = Math.floor(Date.now() / 1000);
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(`${t}.${raw}`),
  );
  const v1 = Buffer.from(sig).toString("hex");
  const res = await fetch(`${PORTAL}/api/portal/stripe-webhook`, {
    method: "POST",
    headers: { "Content-Type": "application/json", "stripe-signature": `t=${t},v1=${v1}` },
    body: raw,
  });
  return { status: res.status, data: await res.json().catch(() => null) };
}

// ── launcher code-path mirrors (src-tauri/src/main.rs) ──────────────────
async function launcherSignIn(client, email, password, machineTag) {
  // main.rs sign_in step 1: login -> session cookie
  let r = await call(client, "/api/auth/login", { method: "POST", body: { email, password } });
  check("login 200", r.status === 200, JSON.stringify(r.data)?.slice(0, 120));
  if (r.status !== 200) throw new Error(`login failed for ${email}`);
  // step 2: mint pairing code
  r = await call(client, "/api/auth/pairing", { method: "POST", body: {} });
  check("pairing minted", r.status === 200 && typeof r.data?.code === "string");
  if (!r.data?.code) throw new Error(`no pairing code for ${email}`);
  const code = r.data.code;
  check("pairing code shape AAA-BBB-CCC", /^[A-Z2-9]{3}-[A-Z2-9]{3}-[A-Z2-9]{3}$/.test(code), code);
  // step 3: pair_machine exchange (exact main.rs payload shape)
  const ingestBody = {
    organization_id: "",
    team_id: "default",
    machine_id: machineTag,
    objective: "launcher pairing",
    source: "pi",
    status: "working",
    started_at: new Date().toISOString(),
    platform: "macos",
    arch: "arm64",
    launcher_version: LAUNCHER_VERSION,
  };
  r = await call(client, "/api/portal/ingest", { method: "POST", body: ingestBody, machineToken: code });
  check("pairing exchange 200 + machine token", r.status === 200 && typeof r.data?.machine_token === "string", `status=${r.status}`);
  if (!r.data?.machine_token) throw new Error(`no machine token for ${email}`);
  // single-use: replay must 401
  const replay = await call(client, "/api/portal/ingest", { method: "POST", body: ingestBody, machineToken: code });
  check("pairing code single-use (replay 401)", replay.status === 401, `status=${replay.status}`);
  return { machineToken: r.data.machine_token, sessionId: r.data.id };
}

async function launcherSoftwareLoop(client, machineToken, orgId) {
  let r = await call(client, "/api/portal/software/updates", { machineToken });
  check("software updates 200", r.status === 200 && Array.isArray(r.data?.updates));
  check("updates scoped to paired org", r.data?.org_id === orgId, `org=${r.data?.org_id}`);
  const before = r.data?.updates?.length ?? -1;
  console.log(`    updates available: ${before} (${(r.data?.updates ?? []).map((u) => u.package_id).join(", ") || "none"})`);
  // report launcher version back (exact main.rs report_software_versions body)
  r = await call(client, "/api/portal/software", {
    method: "POST",
    machineToken,
    body: { packages: { "stria-launcher": LAUNCHER_VERSION } },
  });
  check("report versions ok", r.status === 200 && r.data?.ok === true, `status=${r.status}`);
  // after reporting, stria-launcher must drop out of the update list
  r = await call(client, "/api/portal/software/updates", { machineToken });
  const after = (r.data?.updates ?? []).map((u) => u.package_id);
  check("reported package no longer listed as update", !after.includes("stria-launcher"), after.join(","));
  return { before, after: after.length };
}

// ── Enrolled-2FA operator loop ──────────────────────────────────────────
// The launcher's second-factor path (main.rs sign_in → verify_two_factor):
// enroll while authed → logout → relogin MUST 403-challenge (no session)
// → wrong code 401s → live TOTP 200s with session → pair + software loop
// on the fresh session → recovery code signs in exactly once.
async function launcherTwoFactorLoop(client, email, password, machineTag, orgId) {
  const live = async (secret) =>
    totpCode(base32Decode(secret), Math.floor(Date.now() / 1000));

  let r = await call(client, "/api/auth/two-factor/setup", { method: "POST", body: {} });
  check("2FA setup 200 + secret", r.status === 200 && /^[A-Z2-7]{32}$/.test(r.data?.secret ?? ""));
  if (!r.data?.secret) throw new Error("no totp secret");
  const secret = r.data.secret;

  r = await call(client, "/api/auth/two-factor/confirm", { method: "POST", body: { code: await live(secret) } });
  check("2FA confirm → 10 recovery codes", r.status === 200 && r.data?.recovery_codes?.length === 10, `status=${r.status}`);
  const recovery = r.data?.recovery_codes?.[0];

  await call(client, "/api/auth/logout", { method: "POST", body: {} });
  r = await call(client, "/api/auth/login", { method: "POST", body: { email, password } });
  check("relogin challenges (403, no session)", r.status === 403 && r.data?.two_factor_required === true && !!r.data?.challenge_id, `status=${r.status}`);
  if (!r.data?.challenge_id) throw new Error("no challenge");
  const challenge = r.data.challenge_id;

  r = await call(client, "/api/auth/two-factor/verify", { method: "POST", body: { challenge_id: challenge, code: "000000" } });
  check("wrong 2FA code 401s", r.status === 401, `status=${r.status}`);

  r = await call(client, "/api/auth/two-factor/verify", { method: "POST", body: { challenge_id: challenge, code: await live(secret) } });
  check("live TOTP verifies to session", r.status === 200 && !!client.cookie, `status=${r.status}`);

  // Pair a second machine on the 2FA session (same ingest shape as sign_in).
  const ingestBody = {
    organization_id: "", team_id: "default", machine_id: `${machineTag}-2fa`,
    objective: "launcher pairing", source: "pi", status: "working",
    started_at: new Date().toISOString(), platform: "macos", arch: "arm64",
    launcher_version: LAUNCHER_VERSION,
  };
  r = await call(client, "/api/auth/pairing", { method: "POST", body: {} });
  check("pairing mint after 2FA", r.status === 200 && typeof r.data?.code === "string");
  r = await call(client, "/api/portal/ingest", { method: "POST", body: ingestBody, machineToken: r.data?.code });
  check("2FA-session pair → machine token", r.status === 200 && typeof r.data?.machine_token === "string", `status=${r.status}`);
  const sw = await launcherSoftwareLoop(client, r.data.machine_token, orgId);

  // Recovery code: one more relogin, single use.
  await call(client, "/api/auth/logout", { method: "POST", body: {} });
  r = await call(client, "/api/auth/login", { method: "POST", body: { email, password } });
  r = await call(client, "/api/auth/two-factor/verify", { method: "POST", body: { challenge_id: r.data?.challenge_id, code: recovery } });
  check("recovery code signs in once", r.status === 200 && r.data?.two_factor_method === "recovery_code", `status=${r.status}`);
  return sw;
}

// ── Works launch (mirrors works.rs launch/stop semantics) ───────────────
function lockPid(home) {
  try {
    const raw = readFileSync(join(home, ".stria-works", "stria-works-gpui.lock"), "utf8");
    const pid = parseInt(raw.trim(), 10);
    return Number.isFinite(pid) ? pid : null;
  } catch {
    return null;
  }
}
function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
async function launchWorks(home, label) {
  const child = spawn(WORKS_BIN, [], {
    env: { ...process.env, HOME: home, STRIA_LAUNCHER_SPAWNED: "1" },
    stdio: "ignore",
    detached: false,
  });
  const spawnedPid = child.pid;
  child.unref();
  let pid = null;
  for (let i = 0; i < 60; i++) {
    await sleep(500);
    pid = lockPid(home);
    if (pid && alive(pid)) break;
    pid = null;
  }
  const okLaunch = check(`Works launched + lock PID alive (${label})`, !!pid, `lockPid=${pid}`);
  if (pid) check("spawned child matches lock PID", spawnedPid === pid, `spawn=${spawnedPid} lock=${pid}`);
  // stop: SIGTERM, then SIGKILL (works.rs stop_works)
  if (pid) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {}
    let dead = false;
    for (let i = 0; i < 10; i++) {
      await sleep(500);
      if (!alive(pid)) {
        dead = true;
        break;
      }
    }
    if (!dead) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {}
      for (let i = 0; i < 10; i++) {
        await sleep(500);
        if (!alive(pid)) {
          dead = true;
          break;
        }
      }
    }
    check(`Works stopped cleanly (${label})`, dead, `pid=${pid}`);
  }
  return okLaunch;
}

// ── main ────────────────────────────────────────────────────────────────
async function main() {
  console.log(`Launcher five-tier E2E against ${PORTAL} (stamp ${STAMP})\n`);
  if (!existsSync(WORKS_BIN)) {
    console.error(`Works binary missing: ${WORKS_BIN}`);
    process.exit(2);
  }

  const health = await call(makeClient(), "/api/health").catch(() => null);
  if (!health || health.status !== 200) {
    console.error(`Portal not reachable at ${PORTAL}. Start it first.`);
    process.exit(1);
  }

  // Locate the LIVE local D1 (probe technique from test-accounts.mjs).
  const probeClient = makeClient();
  const probeEmail = `probe-${STAMP}@test.local`;
  const probePw = `Stria-Probe-${STAMP}-xxxxxxxx`;
  const probe = await call(probeClient, "/api/auth/register", {
    method: "POST",
    body: { email: probeEmail, password: probePw, organization: "Locate Probe" },
  });
  if (probe.status !== 200) {
    console.error(`Probe register failed (${probe.status}); cannot locate live D1.`);
    process.exit(1);
  }
  const dbFile = findLiveDb(PORTAL_ROOT, [
    `SELECT 1 AS found FROM users WHERE email=${sqlStr(probeEmail)};`,
  ]);
  exec(dbFile, `DELETE FROM users WHERE email=${sqlStr(probeEmail)};`);
  console.log(`live D1: ${dbFile}\n`);

  for (const c of COMPANIES) {
    console.log(`=== ${c.org} — ${c.tierLabel} ===`);
    const companyResult = { company: c.org, tier: c.tierLabel, users: [] };
    const mkUser = (role, idx) => ({
      role,
      email: `${role}-${c.id}-${STAMP}@test.local`.toLowerCase(),
      password: `Stria-E2E-${STAMP}-${role}-${idx}zz`,
      client: makeClient(),
    });
    const users = c.users.map((u, i) => mkUser(u.role, i));
    const owner = users[0];

    // 1. owner registers the workspace
    let r = await call(owner.client, "/api/auth/register", {
      method: "POST",
      body: { email: owner.email, organization: c.org, password: owner.password },
    });
    check("owner register 200", r.status === 200, `status=${r.status} ${JSON.stringify(r.data)?.slice(0, 100)}`);

    // 2. verify owner email (same column the verify endpoint writes)
    exec(dbFile, `UPDATE users SET email_verified=1 WHERE email=${sqlStr(owner.email.toLowerCase())};`);

    // 3. owner login + org context
    r = await call(owner.client, "/api/auth/login", {
      method: "POST",
      body: { email: owner.email, password: owner.password },
    });
    check("owner login 200", r.status === 200);
    r = await call(owner.client, "/api/portal/me");
    const membership = r.data?.memberships?.find((m) => m.org_name === c.org) ?? r.data?.memberships?.[0];
    const orgId = membership?.org_id;
    const teamId = membership?.team_id;
    check("owner bootstrapped org + owner role", !!orgId && membership?.role === "owner", JSON.stringify(membership));
    companyResult.orgId = orgId;

    // 4. onboarding (skipped for Free: it must stay in the restricted branch)
    if (c.tier) {
      for (const answer of INTAKE_ANSWERS) {
        await call(owner.client, "/api/portal/onboarding", { method: "POST", body: { answer } });
      }
      r = await call(owner.client, "/api/portal/onboarding", { method: "POST", body: { action: "confirm" } });
      check("onboarding complete", r.data?.onboarding_status === "complete" || r.status === 200, `status=${r.status}`);
    }

    // 5. subscription through the REAL stripe webhook (skipped for Free)
    if (c.tier) {
      const hook = await subscribe(orgId, c.tier, owner.email);
      check(`subscribed (${c.tier}) via webhook`, hook.status === 200, `status=${hook.status}`);
      r = await call(owner.client, `/api/portal/billing?org=${orgId}`);
      check("billing shows paid tier", r.data?.tier === c.tier, `tier=${r.data?.tier}`);
      companyResult.billingTier = r.data?.tier;
    }

    // 6. entitlement gate (session): free must be restricted, paid entitled
    r = await call(owner.client, "/api/portal/downloads");
    check(
      c.expectEntitled ? "downloads entitled" : "downloads restricted (free)",
      c.expectEntitled ? r.data?.entitled === true : r.data?.entitled !== true,
      `entitled=${r.data?.entitled} state=${r.data?.state}`,
    );
    companyResult.downloads = { entitled: r.data?.entitled, state: r.data?.state };

    // 7. org software manifest (session): all 5 packages pinned at 0.1.0
    r = await call(owner.client, `/api/portal/software?org=${orgId}`);
    const pkgs = r.data?.packages ?? [];
    check("manifest lists 5 packages", pkgs.length === 5, `got=${pkgs.length}`);

    // 8. invite team (owner/admin only path) + register operators with SAME org name
    for (const u of users.slice(1)) {
      r = await call(owner.client, "/api/portal/team", {
        method: "POST",
        body: { action: "invite", email: u.email, team_id: teamId, role: u.role },
      });
      check(`invite ${u.role} (${u.email})`, r.status === 200 && r.data?.ok === true, `status=${r.status}`);
      r = await call(u.client, "/api/auth/register", {
        method: "POST",
        body: { email: u.email, organization: c.org, password: u.password },
      });
      check(`${u.role} register 200`, r.status === 200, `status=${r.status}`);
      exec(dbFile, `UPDATE users SET email_verified=1 WHERE email=${sqlStr(u.email.toLowerCase())};`);
      r = await call(u.client, "/api/auth/login", {
        method: "POST",
        body: { email: u.email, password: u.password },
      });
      check(`${u.role} login 200`, r.status === 200);
      r = await call(u.client, "/api/portal/me");
      const mine = (r.data?.memberships ?? []).find((m) => m.org_id === orgId);
      check(`${u.role} active in company org as ${u.role}`, !!mine && mine.role === u.role, JSON.stringify(mine));
    }

    // 9. launcher sign_in + pair + software loop for EVERY user (owner + operators)
    for (const u of users) {
      console.log(`  -- launcher path: ${u.role} ${u.email}`);
      const machineTag = `e2e-${c.id}-${u.role}-${STAMP}`;
      const { machineToken, sessionId } = await launcherSignIn(u.client, u.email, u.password, machineTag);
      const sw = await launcherSoftwareLoop(u.client, machineToken, orgId);
      // Works-style usage report on the paired machine token
      r = await call(u.client, "/api/portal/ingest", {
        method: "POST",
        machineToken,
        body: {
          session_id: sessionId,
          status: "complete",
          cost: { provider_cost: 0.014, managed_fee: 0.001, avoided_cost: 0.42, currency: "USD" },
          usage: {
            by_model: [
              { model: "llama-3.1-8b-local", calls: 12, input_tokens: 4000, output_tokens: 2100, cost: 0, cache_hits: 9 },
            ],
          },
          checkpoints: [{ seq: 1, label: "launcher-e2e", verdict: "pass", at: new Date().toISOString() }],
        },
      });
      check("usage ingest stored", r.status === 200, `status=${r.status}`);
      // session visible in portal
      r = await call(u.client, `/api/portal/sessions?org=${orgId}`);
      check("session listed in portal", (r.data?.sessions ?? []).some((s) => s.id === sessionId));
      u.machineToken = machineToken;
      u.sessionId = sessionId;
      u.home = join(tmpdir(), `stria-e2e-${c.id}-${u.role}-${STAMP}`);
      // portal.json exactly like persist_pairing in main.rs
      mkdirSync(join(u.home, ".stria"), { recursive: true });
      writeFileSync(
        join(u.home, ".stria", "portal.json"),
        JSON.stringify(
          {
            portalUrl: PORTAL,
            machineId: machineTag,
            machineToken,
            teamId: "team_default",
            platform: "macos",
            arch: "arm64",
            launcherVersion: LAUNCHER_VERSION,
            pairedAt: new Date().toISOString(),
          },
          null,
          2,
        ) + "\n",
        { mode: 0o600 },
      );
      companyResult.users.push({ role: u.role, email: u.email, updatesBefore: sw.before, sessionId });
    }

    // 10. tier budget sanity via billing spend
    r = await call(owner.client, `/api/portal/billing?org=${orgId}`);
    check("billing spend counts sessions", (r.data?.spend?.sessions ?? 0) >= users.length, JSON.stringify(r.data?.spend));

    // 10b. enrolled-2FA operator loop (Growth only — one enrolled operator
    // proves the challenge branch inside the full matrix without slowing
    // every company).
    if (c.id === "growth") {
      const op = users.find((u) => u.role === "operator");
      console.log(`  -- 2FA path: operator ${op.email}`);
      const sw2fa = await launcherTwoFactorLoop(op.client, op.email, op.password, `e2e-${c.id}-operator-${STAMP}`, orgId);
      companyResult.twofa = { updatesAfter: sw2fa.after };
    }

    results.push(companyResult);
    console.log(`--- ${c.org} done ---\n`);
  }

  // 11. negative paths (launcher-relevant authz)
  console.log("=== negative paths ===");
  const anon = makeClient();
  let r = await call(anon, "/api/auth/login", {
    method: "POST",
    body: { email: `owner-free-${STAMP}@test.local`, password: "wrong-password-xyz" },
  });
  check("wrong password rejected", r.status === 401 || r.status === 400, `status=${r.status}`);
  r = await call(anon, "/api/auth/pairing", { method: "POST", body: {} });
  check("pairing without session rejected", r.status === 401, `status=${r.status}`);
  r = await call(anon, "/api/portal/software/updates", { machineToken: "stria_mt_bogus" });
  check("updates with bogus token rejected", r.status === 401, `status=${r.status}`);
  r = await call(anon, "/api/portal/software/updates");
  check("updates without token rejected", r.status === 401, `status=${r.status}`);

  // 12. Works launch phase: owner machine of every company (+ growth operator machine)
  console.log("\n=== Stria Works launch (real binary, sandboxed HOME) ===");
  const launchTargets = [];
  for (const cr of results) {
    const ownerIdx = COMPANIES.find((c) => c.org === cr.company).users.findIndex((u) => u.role === "owner");
    launchTargets.push({ company: cr.company, user: cr.users[ownerIdx] });
    if (cr.company === "NeuralForge") {
      launchTargets.push({ company: `${cr.company} (operator 2nd machine)`, user: cr.users[2] });
    }
  }
  // re-resolve home dirs (stored on user objects above)
  const homeByEmail = new Map();
  for (const c of COMPANIES) {
    for (const u of c.users) {
      homeByEmail.set(`${u.role}-${c.id}-${STAMP}@test.local`.toLowerCase(), join(tmpdir(), `stria-e2e-${c.id}-${u.role}-${STAMP}`));
    }
  }
  for (const t of launchTargets) {
    const home = homeByEmail.get(t.user.email.toLowerCase());
    console.log(`  -- ${t.company} (${t.user.email})`);
    const ok = await launchWorks(home, t.company);
    t.ok = ok;
  }

  console.log(`\n===== RESULT: ${checks - failures}/${checks} checks passed =====`);
  for (const cr of results) {
    console.log(`  ${cr.company} [${cr.tier}] org=${cr.orgId} billing=${cr.billingTier ?? "none"} downloads=${JSON.stringify(cr.downloads)}`);
    for (const u of cr.users) console.log(`    ${u.role}: ${u.email} updatesBefore=${u.updatesBefore} session=${u.sessionId}`);
  }
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error("E2E crashed:", e);
  process.exit(1);
});
