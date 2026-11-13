const test = require("node:test");
const assert = require("node:assert");
const { dedupeBy } = require("./dedupe");

test("keeps the first item per key", () => {
  const rows = [{ id: 1, v: "a" }, { id: 2, v: "b" }, { id: 1, v: "c" }];
  assert.deepStrictEqual(dedupeBy(rows, (r) => r.id), [rows[0], rows[1]]);
});

test("works on primitives", () => {
  assert.deepStrictEqual(dedupeBy([3, 1, 3, 2, 1], (x) => x), [3, 1, 2]);
});

test("empty input", () => {
  assert.deepStrictEqual(dedupeBy([], (x) => x), []);
});
