const test = require("node:test");
const assert = require("node:assert");
const { debounce } = require("./debounce");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test("calls once after the quiet period with the last arguments", async () => {
  const calls = [];
  const d = debounce((x) => calls.push(x), 30);
  d(1); d(2); d(3);
  assert.deepStrictEqual(calls, []);
  await sleep(80);
  assert.deepStrictEqual(calls, [3]);
});

test("a later call restarts the timer", async () => {
  const calls = [];
  const d = debounce((x) => calls.push(x), 40);
  d("a");
  await sleep(25);
  d("b");
  await sleep(25);
  assert.deepStrictEqual(calls, []);
  await sleep(50);
  assert.deepStrictEqual(calls, ["b"]);
});

test("cancel prevents the call", async () => {
  const calls = [];
  const d = debounce(() => calls.push(1), 20);
  d();
  d.cancel();
  await sleep(50);
  assert.deepStrictEqual(calls, []);
});

test("keeps `this`", async () => {
  const obj = { n: 7, seen: null, go: debounce(function () { obj.seen = this.n; }, 10) };
  obj.go();
  await sleep(40);
  assert.strictEqual(obj.seen, 7);
});
