import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";

const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("MultiCLI enables only its Windows verification workflow, without inherited publication jobs", () => {
  const workflows = readdirSync(new URL("../.github/workflows/", import.meta.url)).filter((name) => /\.ya?ml$/.test(name));
  assert.deepEqual(workflows, ["multicli-windows.yml"]);
  const workflow = read(".github/workflows/multicli-windows.yml");
  assert.match(workflow, /runs-on: windows-latest/);
  assert.match(workflow, /contents: read/);
  assert.doesNotMatch(workflow, /contents: write|pull-requests: write|secrets\.|create-release|gh release/);
});

test("Windows MSVC embeds the manifest for library tests as well as the application", () => {
  const build = read("src-tauri/build.rs");
  assert.match(build, /CARGO_CFG_TARGET_OS/);
  assert.match(build, /CARGO_CFG_TARGET_ENV/);
  assert.match(build, /new_without_app_manifest\(\)/);
  assert.match(build, /cargo:rustc-link-arg=\/MANIFEST:EMBED/);
  assert.match(build, /cargo:rustc-link-arg=\/MANIFESTINPUT:/);
  assert.match(build, /cargo:rerun-if-changed=/);
  assert.doesNotMatch(build, /rustc-link-arg-(?:bins|tests)=/);
  const manifest = read("src-tauri/windows-app-manifest.xml");
  assert.match(manifest, /name="Microsoft.Windows.Common-Controls"/);
  assert.match(manifest, /version="6\.0\.0\.0"/);
  assert.match(manifest, /processorArchitecture="\*"/);
  assert.match(manifest, /publicKeyToken="6595b64144ccf1df"/);
});

test("Windows verification requires locked Rust checks and real MultiCLI tests before packaging", () => {
  const workflow = read(".github/workflows/multicli-windows.yml");
  assert.match(workflow, /verify\.ps1 -Scope All/);
  assert.doesNotMatch(workflow, /continue-on-error: true/);
  const verify = read("scripts/multicli/verify.ps1");
  assert.match(verify, /cargo check --locked/);
  assert.match(verify, /cargo test --locked.*--lib multicli_bootstrap/);
  assert.match(verify, /cargo test --locked.*--lib 'multicli::'/);
  assert.match(verify, /cargo test --locked.*--example multicli_fixture/);
  assert.match(verify, /pnpm exec vitest run src\/multicli/);
  assert.match(verify, /tauri build --bundles nsis --no-sign/);
  assert.ok(verify.indexOf("cargo test") < verify.indexOf("tauri build"));
  assert.match(verify, /throw 'MultiCLI native tests failed'/);
});

// Packaged builds apply their CSP plus a build step that tags every inline
// <style>/<script> in index.html with a __TAURI_*_NONCE__ token (tauri-codegen
// inject_nonce_token) and appends the nonce to the matching directive at
// runtime (tauri replace_csp_nonce). A nonce cancels 'unsafe-inline' in the
// same directive, so a single inline <style> in index.html makes the webview
// refuse every stylesheet created at runtime — plugin styles.css bundles and
// ctx.theme.injectCss included — with no JS error to surface. v1.0.9 shipped
// the boot splash inline and silently lost all plugin styling in packaged
// builds only (dev is unaffected: the dev HTML never gets the token). Keep
// boot assets external and 'unsafe-inline' in style-src.
test("index.html keeps boot assets external so Tauri's CSP nonce can't disable runtime-injected plugin CSS", () => {
  // Comments may name the tags they forbid; only real elements count.
  const html = read("index.html").replace(/<!--[\s\S]*?-->/g, "");
  assert.doesNotMatch(html, /<style[\s>]/, "inline <style> would nonce style-src and block every plugin stylesheet");
  assert.doesNotMatch(html, /<script(?![^>]*\bsrc=)[^>]*>/, "inline <script> would nonce script-src");
  assert.match(html, /<link rel="stylesheet" href="\/boot\.css"\s*\/?>/, "boot.css must be linked");
  assert.match(read("public/boot.css"), /#boot-splash\b/);
  assert.match(read("src-tauri/tauri.conf.json"), /style-src 'self' 'unsafe-inline'/, "plugin runtime CSS relies on style-src 'unsafe-inline'");
});
