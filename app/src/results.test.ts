import { describe, expect, it } from "vitest";
import { applyBatches, firstVisible, marks, scrollFor, visibleRange, type Item } from "./results";

const item = (id: string): Item => ({ id, title: id, subtitle: "", preview: "", icon: null, actions: [] });

describe("applyBatches", () => {
  it("replaces on a newer query and appends within one", () => {
    const chips = [{ label: "ext: pdf", valid: true }];
    const shown = applyBatches({ id: 1, items: [item("old")], chips: [] }, [
      { id: 2, source: "apps", items: [item("a")], chips },
      { id: 2, source: "files", items: [item("b")], chips: [] },
    ]);
    expect(shown.id).toBe(2);
    expect(shown.items.map((i) => i.id)).toEqual(["a", "b"]);
    expect(shown.chips).toEqual(chips);
  });

  it("drops batches from older queries", () => {
    const shown = applyBatches({ id: 3, items: [item("keep")], chips: [] }, [
      { id: 2, source: "apps", items: [item("stale")], chips: [{ label: "x", valid: true }] },
    ]);
    expect(shown.items.map((i) => i.id)).toEqual(["keep"]);
    expect(shown.chips).toEqual([]);
  });

  it("clears the list when a newer query has no results", () => {
    const next = applyBatches({ id: 1, items: [item("x")], chips: [] }, [{ id: 2, source: "apps", items: [], chips: [] }]);
    expect(next.items).toEqual([]);
  });
});

describe("virtual list", () => {
  it("renders only rows near the viewport", () => {
    expect(visibleRange(440, 220, 44, 1000)).toEqual([6, 19]);
    expect(visibleRange(0, 220, 44, 3)).toEqual([0, 3]);
    expect(visibleRange(0, 220, 44, 0)).toEqual([0, 0]);
  });

  it("scrolls the least needed to show a row", () => {
    expect(scrollFor(2, 0, 220, 44)).toBe(0);
    expect(scrollFor(5, 0, 220, 44)).toBe(44);
    expect(scrollFor(1, 132, 220, 44)).toBe(44);
  });

  it("numbers from the first fully visible row", () => {
    expect(firstVisible(0, 44)).toBe(0);
    expect(firstVisible(10, 44)).toBe(1);
  });
});

describe("marks", () => {
  it("marks query words case-insensitively and ignores short words and filters", () => {
    expect(marks("The Invoice is due", "invoice ext:pdf is")).toEqual([
      { text: "The ", hit: false },
      { text: "Invoice", hit: true },
      { text: " is due", hit: false },
    ]);
    expect(marks("a+b (x)", "(x)")).toEqual([{ text: "a+b ", hit: false }, { text: "(x)", hit: true }]);
    expect(marks("plain", "")).toEqual([{ text: "plain", hit: false }]);
  });
});
