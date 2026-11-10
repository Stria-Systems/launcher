#!/usr/bin/env node
/*
 * Frontend contract for the launcher's single static page.
 *
 * The UI has no build step, so nothing else would catch the failure mode that
 * shipped once: a duplicate `const` made the whole inline script a syntax
 * error, every handler disappeared, and the page still looked fine. This
 * check parses the script, verifies the CSP hash in tauri.conf.json matches
 * the exact bytes Tauri will serve, and confirms the bundled brand fonts
 * exist.
 *
 * Run: node ui/check-ui.mjs
 */
import { createHash } from "node:crypto";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const htmlPath = join(here, "index.html");
const confPath = join(here, "..", "src-tauri", "tauri.conf.json");

const html = readFileSync(htmlPath, "utf8");
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
if (scripts.length !== 1) {
  fail(`expected exactly one inline script, found ${scripts.length}`);
}
const source = scripts[0][1];

try {
  new vm.Script(source, { filename: "ui/index.html:inline-script" });
} catch (error) {
  fail(`inline script does not parse: ${error.message}`);
}

const conf = JSON.parse(readFileSync(confPath, "utf8"));
const csp = conf?.app?.security?.csp ?? "";
const declared = csp.match(/script-src[^;]*'sha256-([^']+)'/)?.[1];
if (!declared) fail("tauri.conf.json CSP has no script-src sha256 hash");

const actual = createHash("sha256").update(source, "utf8").digest("base64");
if (declared !== actual) {
  fail(
    `CSP hash is stale: conf has ${declared}, inline script is ${actual}. ` +
      `Update app.security.csp in src-tauri/tauri.conf.json.`
  );
}

const fonts = [...html.matchAll(/url\("(fonts\/[^"]+)"\)/g)].map((m) => m[1]);
if (fonts.length === 0) fail("no bundled fonts are referenced");
for (const font of fonts) {
  if (!existsSync(join(here, font))) fail(`referenced font missing: ${font}`);
}

const requiredIds = ["setup", "hub", "env-pill", "signin-form", "pairing-toggle", "resend-email"];
for (const id of requiredIds) {
  if (!html.includes(`id="${id}"`)) fail(`required element missing: #${id}`);
}

console.log(
  `launcher ui ok · script ${source.length} bytes · ${fonts.length} fonts · csp hash matches`
);

function fail(message) {
  console.error(`launcher ui check failed: ${message}`);
  process.exit(1);
}
