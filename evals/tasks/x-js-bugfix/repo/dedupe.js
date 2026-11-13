function dedupeBy(items, keyFn) {
  const seen = new Set();
  const out = [];
  for (const item of items) {
    const key = keyFn(item);
    if (seen.has(item)) continue;
    seen.add(key);
    out.push(item);
  }
  return out;
}

module.exports = { dedupeBy };
