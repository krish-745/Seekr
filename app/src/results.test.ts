import { describe, expect, it } from "vitest";
import { applyBatches, firstVisible, scrollFor, visibleRange, type Item } from "./results";

const item = (id: string): Item => ({ id, title: id, subtitle: "", preview: "", actions: [] });

describe("applyBatches", () => {
  it("replaces on a newer query and appends within one", () => {
    const shown = applyBatches({ id: 1, items: [item("old")] }, [
      { id: 2, items: [item("a")] },
      { id: 2, items: [item("b")] },
    ]);
    expect(shown.id).toBe(2);
    expect(shown.items.map((i) => i.id)).toEqual(["a", "b"]);
  });

  it("drops batches from older queries", () => {
    const shown = applyBatches({ id: 3, items: [item("keep")] }, [{ id: 2, items: [item("stale")] }]);
    expect(shown.items.map((i) => i.id)).toEqual(["keep"]);
  });

  it("clears the list when a newer query has no results", () => {
    expect(applyBatches({ id: 1, items: [item("x")] }, [{ id: 2, items: [] }]).items).toEqual([]);
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
