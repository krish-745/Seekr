// README screenshots from the real app: `scripts\smoke.ps1 -Script app\e2e\screenshots.mjs`.
// A hidden webview doesn't paint, so this opens the panel with the test hotkey (it shows on
// screen while the shots are taken) and captures the panel's own page, never the desktop. The
// queries are chosen to show nothing personal, but check every image before committing it.
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const [server, application] = process.argv.slice(2);
const OUT = fileURLToPath(new URL("../../docs/screenshots/", import.meta.url));
const HOTKEY = fileURLToPath(new URL("hotkey.ps1", import.meta.url));
const KEY = { Enter: "", Escape: "", ArrowDown: "" };

// A solid dark panel: Mica and acrylic show the desktop through, which a capture can't. The web
// tool is off: it would list the user's own bookmarks and history.
const config = `${process.env.SEEKR_CONFIG_DIR}\\config.toml`;
writeFileSync(config, readFileSync(config, "utf8").replace(/\[tools\]\r?\n/, "[tools]\nweb = false\n"));
appendFileSync(config, '\n[theme]\nmode = "dark"\nbackdrop = "solid"\n');

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
const js = (script) => call("POST", session("/execute/sync"), { script, args: [] });
const find = async (css) => Object.values(await call("POST", session("/element"), { using: "css selector", value: css }))[0];
const keys = (element, text) => call("POST", session(`/element/${element}/value`), { text });
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
async function until(what, script) {
  const start = Date.now();
  while (!(await js(script))) {
    if (Date.now() - start > 15000) throw new Error(`timed out waiting for ${what}`);
    await pause(100);
  }
}

async function shot(name) {
  await pause(600);
  const png = await call("GET", session("/screenshot"));
  writeFileSync(`${OUT}${name}.png`, Buffer.from(png, "base64"));
  console.log(`wrote docs/screenshots/${name}.png`);
}

// Typed again until `ready` shows: a fresh data folder has no currency rates, and the
// calculator answers without them while they download.
async function search(text, ready) {
  const found = `return [...document.querySelectorAll('.row')].some(r => r.textContent.includes(${JSON.stringify(ready)}))`;
  for (let attempt = 0; attempt < 8; attempt++) {
    await js("const q = document.querySelector('input.query'); q.value = ''; q.dispatchEvent(new Event('input', { bubbles: true }))");
    await keys(await find("input.query"), text);
    for (let i = 0; i < 20; i++) {
      if (await js(found)) return;
      await pause(100);
    }
  }
  throw new Error(`no "${ready}" for ${text}`);
}

try {
  mkdirSync(OUT, { recursive: true });
  const pressed = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", HOTKEY, "-Press"], { encoding: "utf8" });
  if (pressed.status !== 0) throw new Error(`hotkey: ${pressed.status === 3 ? "input never idle" : pressed.stderr}`);
  await until("the panel", "return document.hasFocus()");

  await search("(1200 * 1.18) / 12", "118");
  await shot("calculator");
  await search("250 usd in inr", "INR");
  await shot("currency");
  await search(":fire", "fire");
  await shot("emoji");
  // Apps, not files: file results would show paths under the user's own profile.
  await search("paint", "Paint");
  await shot("apps");
  await search("seekr settings", "Seekr settings");
  const chosen = () => js("return !!document.querySelector('.row[aria-selected=true]')?.textContent.includes('Seekr settings')");
  for (let i = 0; i < 10 && !(await chosen()); i++) await keys(await find("input.query"), KEY.ArrowDown);
  if (!(await chosen())) throw new Error("Seekr settings isn't selected");
  await keys(await find("input.query"), KEY.Enter);
  await until("settings", "return document.querySelector('[aria-label=Settings] [role=option]')");
  // Down to the tools, past the test instance's own hotkey.
  for (let i = 0; i < 6; i++) await keys(await find("[aria-label=Settings]"), KEY.ArrowDown);
  await shot("settings");
  await keys(await find("[aria-label=Settings]"), KEY.Escape);
  await keys(await find("input.query"), KEY.Escape);
} finally {
  await call("DELETE", session(""));
}
