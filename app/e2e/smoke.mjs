// WebDriver smoke test of the built app, run by scripts/smoke.ps1 through tauri-driver.
// Until the last check the panel stays hidden: keys go to the webview through WebDriver, not
// the desktop, so nothing reaches other windows. Nothing is launched or copied.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const [server, application] = process.argv.slice(2);
const HOTKEY = fileURLToPath(new URL("hotkey.ps1", import.meta.url));
const KEY = { Enter: "", Tab: "", Escape: "", ArrowDown: "" };

async function call(method, path, body) {
  const response = await fetch(server + path, {
    method,
    headers: { "Content-Type": "application/json" },
    body: body && JSON.stringify(body),
  });
  const { value } = await response.json();
  if (!response.ok) throw new Error(`${method} ${path}: ${value?.message ?? response.status}`);
  return value;
}

const { sessionId } = await call("POST", "/session", {
  capabilities: { alwaysMatch: { browserName: "wry", "tauri:options": { application, args: ["--hidden"] } } },
});
const session = (path) => `/session/${sessionId}${path}`;
const js = (script, ...args) => call("POST", session("/execute/sync"), { script, args });
// The reference is keyed "element-6066-…" in W3C replies and "ELEMENT" in older ones.
const find = async (css) => Object.values(await call("POST", session("/element"), { using: "css selector", value: css }))[0];
const keys = (element, text) => call("POST", session(`/element/${element}/value`), { text });
// WebDriver's own clear refuses elements in a hidden window, so empty it the way typing would.
const clear = () =>
  js("const q = document.querySelector('input.query'); q.value = ''; q.dispatchEvent(new Event('input', { bubbles: true }))");

// Polls a script until it returns something truthy, so the test waits only as long as Seekr does.
async function until(what, script, ...args) {
  const start = Date.now();
  for (;;) {
    const value = await js(script, ...args);
    if (value) return value;
    if (Date.now() - start > 15000) throw new Error(`timed out waiting for ${what}`);
    await new Promise((done) => setTimeout(done, 100));
  }
}

const passed = [];
async function check(name, body) {
  const start = Date.now();
  await body();
  passed.push(name);
  console.log(`ok - ${name} (${Date.now() - start} ms)`);
}

try {
  const input = await find("input.query");
  const active = () => js("return document.querySelector('input.query').getAttribute('aria-activedescendant')");

  await check("the calculator answers as you type", async () => {
    await keys(input, "12*12");
    await until("144", "return [...document.querySelectorAll('.row')].some(r => r.textContent.includes('144'))");
  });

  await check("arrow keys move the selection screen readers follow", async () => {
    await clear();
    await keys(input, "seekr");
    await until("two results", "return document.querySelectorAll('.row').length >= 2");
    // Those may be for "seek": the results for "seekr" replace them and select the first again.
    await new Promise((done) => setTimeout(done, 500));
    assert.equal(await active(), "row-0");
    await keys(input, KEY.ArrowDown);
    assert.equal(await active(), "row-1");
  });

  await check("Tab opens the actions menu and Esc closes it", async () => {
    await keys(input, KEY.Tab);
    await until("the actions menu", "return document.querySelector('.actions [role=option]')");
    assert.equal(await active(), "action-0");
    await keys(input, KEY.Escape);
    await until("the menu to close", "return !document.querySelector('.actions [role=option]')");
  });

  await check("Seekr settings opens the settings view", async () => {
    await clear();
    await keys(input, "seekr settings");
    await until("the Seekr settings row", "return [...document.querySelectorAll('.row')].some(r => r.textContent.includes('Seekr settings'))");
    // Let any later batch of results land: Enter on a neighbor such as "Quit Seekr" must not happen.
    await new Promise((done) => setTimeout(done, 500));
    const chosen = () => js("return !!document.querySelector('.row[aria-selected=true]')?.textContent.includes('Seekr settings')");
    for (let i = 0; i < 10 && !(await chosen()); i++) await keys(input, KEY.ArrowDown);
    assert.ok(await chosen(), "Seekr settings is selected");
    await keys(input, KEY.Enter);
    await until("the settings list", "return document.querySelector('[aria-label=Settings] [role=option]')");
  });

  await check("Diagnostics opens from settings and shows the index counts", async () => {
    const list = await find("[aria-label=Settings]");
    const index = await js(
      "return [...document.querySelectorAll('[aria-label=Settings] [role=option]')].findIndex(li => li.textContent.includes('Diagnostics'))",
    );
    assert.ok(index >= 0, "Diagnostics is listed");
    for (let i = 0; i < index; i++) await keys(list, KEY.ArrowDown);
    assert.equal(await js("return document.querySelector('[aria-label=Settings]').getAttribute('aria-activedescendant')"), `setting-${index}`);
    await keys(list, KEY.Enter);
    await until("diagnostics", "return document.querySelector('section.diagnostics table')");
    const pane = await find("section.diagnostics");
    await keys(pane, KEY.Escape);
    await until("settings again", "return !document.querySelector('section.diagnostics') && document.querySelector('[aria-label=Settings]')");
    await keys(await find("[aria-label=Settings]"), KEY.Escape);
    await until("the results again", "return !document.querySelector('[aria-label=Settings]') && document.querySelector('.row')");
  });

  // The one check that shows the panel on screen, for about a second: real keys, real focus.
  await check("the hotkey shows the panel, and Esc hides it and hands focus back", async () => {
    const run = (...args) =>
      spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", HOTKEY, ...args], { encoding: "utf8" });
    const pressed = run("-Press");
    if (pressed.status === 3) {
      console.log("skipped: the keyboard or mouse was in use for a minute");
      return;
    }
    assert.equal(pressed.status, 0, pressed.stderr);
    const before = pressed.stdout.trim();
    await until("the panel", "return document.hasFocus() && document.activeElement === document.querySelector('input.query')");
    await keys(input, KEY.Escape);
    await until("the panel to hide", "return !document.hasFocus()").catch(async (e) => {
      // Failed once in about eight runs; this says what the panel was doing.
      const state = await js(
        "return JSON.stringify({ focused: document.hasFocus(), visibility: document.visibilityState, active: document.activeElement?.className, covered: !!document.querySelector('section.setup'), query: document.querySelector('input.query').value })",
      );
      throw new Error(`${e.message}: ${state}`);
    });
    assert.equal(run().stdout.trim(), before, "focus went back to the window used before");
  });

  console.log(`${passed.length} passed`);
} finally {
  await call("DELETE", session(""));
}
