// @vitest-environment jsdom
// The panel rendered with stand-ins for Seekr's Rust side: axe finds no accessibility problems,
// and screen readers can follow the selection by keyboard.
import axe from "axe-core";
import { render } from "solid-js/web";
import { afterAll, beforeAll, expect, test } from "vitest";

const callbacks: Record<number, (message: unknown) => void> = {};
let next = 1;
const item = (n: number, title: string, subtitle: string, actions = [{ id: "open", label: "Open" }]) => ({
  id: `app:${n}`,
  title,
  subtitle,
  preview: subtitle,
  icon: null,
  actions,
});
const field = (key: string, group: string, label: string, kind: string, value: unknown, hint = "") => ({
  key,
  group,
  label,
  kind,
  value,
  hint,
});

beforeAll(() => {
  Object.assign(window, {
    __TAURI_INTERNALS__: {
      transformCallback(cb: (message: unknown) => void) {
        const id = next++;
        callbacks[id] = cb;
        return id;
      },
      unregisterCallback(id: number) {
        delete callbacks[id];
      },
      async invoke(cmd: string, args: { id: number; text: string; onBatch: { id: number } }) {
        if (cmd === "query") {
          const items =
            args.text === "settings"
              ? [item(9, "Seekr settings", "Hotkey, tools", [{ id: "settings", label: "Open" }])]
              : [item(1, "Notepad", "App"), item(2, "notes.md", "C:\\Users\\me\\Documents"), item(3, "Notion", "App")];
          setTimeout(() => callbacks[args.onBatch.id]({ index: 0, message: { id: args.id, source: "apps", items, chips: [] } }));
          return null;
        }
        if (cmd === "settings")
          return {
            path: "C:\\Users\\me\\AppData\\Roaming\\Seekr\\config.toml",
            problem: null,
            css: null,
            fields: [
              field("hotkey", "General", "Hotkey", "text", "Alt+Space", "Such as Alt+Space"),
              field("tools.emoji", "Tools", "Emoji and symbols", "toggle", true),
              field("prefixes.clipboard", "Prefixes", "Clipboard history", "text", "clip"),
              field("theme.mode", "Appearance", "Light or dark", "choice", "system"),
              field("theme.font_size", "Appearance", "Font size", "number", 14),
              field("theme.density", "Appearance", "Row spacing", "choice", "comfortable"),
            ],
          };
        if (cmd === "preview") return { kind: "nothing" };
        if (cmd === "plugin:event|listen") return 1;
        return null;
      },
    },
    ResizeObserver: class {
      observe() {}
      disconnect() {}
    },
    matchMedia: () => ({ matches: false }),
  });
  // jsdom has no layout, so nothing to scroll.
  Element.prototype.scrollIntoView = () => {};
});

let dispose = () => {};
afterAll(() => dispose());

const frames = () => new Promise((done) => setTimeout(done, 50));
const key = (k: string) => window.dispatchEvent(new KeyboardEvent("keydown", { key: k }));
const problems = async () =>
  (await axe.run(document, { rules: { "color-contrast": { enabled: false } } })).violations.map(
    (v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`,
  );

test("results, actions and settings can be followed by a screen reader", async () => {
  const { default: App } = await import("./App");
  const root = document.createElement("div");
  document.body.append(root);
  document.documentElement.lang = "en";
  document.title = "Seekr";
  dispose = render(() => <App />, root);
  const input = document.querySelector<HTMLInputElement>("input.query")!;
  input.value = "no";
  input.dispatchEvent(new Event("input", { bubbles: true }));
  await frames();

  expect(input.getAttribute("role")).toBe("combobox");
  expect(input.getAttribute("aria-activedescendant")).toBe("row-0");
  expect(document.getElementById("row-0")?.getAttribute("aria-selected")).toBe("true");
  key("ArrowDown");
  await frames();
  expect(input.getAttribute("aria-activedescendant")).toBe("row-1");
  expect(document.getElementById("row-1")?.getAttribute("aria-posinset")).toBe("2");
  expect(await problems()).toEqual([]);

  key("Tab");
  await frames();
  expect(input.getAttribute("aria-activedescendant")).toBe("action-0");
  expect(await problems()).toEqual([]);
  key("Escape");

  input.value = "settings";
  input.dispatchEvent(new Event("input", { bubbles: true }));
  await frames();
  key("Enter");
  await frames();
  const list = document.querySelector<HTMLElement>('[aria-label="Settings"]')!;
  expect(document.activeElement).toBe(list);
  expect(list.getAttribute("aria-activedescendant")).toBe("setting-0");
  key("ArrowDown");
  await frames();
  expect(list.getAttribute("aria-activedescendant")).toBe("setting-1");
  expect(document.getElementById("setting-1")?.getAttribute("aria-description")).toBe("Tools");
  expect(await problems()).toEqual([]);
});
