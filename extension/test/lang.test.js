import test from "node:test";
import assert from "node:assert/strict";

// shared.js is injected as a classic script and has no exports; it publishes
// onto the global, which is how content.js reads it too.  Importing it for
// side effects therefore gives the test the same object the browser sees.
import "../src/shared.js";

const { langSpans, utf8ByteLength } = globalThis.ZhtwExtensionShared;

/// Build spans the way collectVisibleText does: one flattened string, byte
/// offsets into it, and the lang each run inherited from its ancestors.  The
/// separator content.js inserts between blocks is written into the fixture as
/// a run of its own so the offsets are the real ones.
function spansOf(...runs) {
  let byteStart = 0;
  return runs.map(([text, lang]) => {
    const byteEnd = byteStart + utf8ByteLength(text);
    const span = { byteStart, byteEnd, text, lang };
    byteStart = byteEnd;
    return span;
  });
}

/// What each reported run names, decoded out of the flattened text.
function runTexts(spans, runs) {
  const text = spans.map((span) => span.text).join("");
  const bytes = Buffer.from(text, "utf8");
  return runs.map((run) => bytes.subarray(run.start, run.end).toString("utf8"));
}

test("a declared run maps to its own byte range and nothing more", () => {
  const spans = spansOf(["他說", "zh-TW"], ["I agree, 但", "en"], ["結束", null]);
  const runs = langSpans(spans);

  // The range has to name the same bytes the flattened string holds, or the
  // scanner silences the wrong run.
  assert.deepEqual(runTexts(spans, runs), ["他說", "I agree, 但"]);
  assert.deepEqual(
    runs.map((run) => run.lang),
    ["zh-TW", "en"],
  );
});

test("consecutive runs under the same declaration fold into one", () => {
  // A page whose html element carries a lang would otherwise send one entry
  // per text node.
  const spans = spansOf(["one", "en"], ["two", "en"], ["三", "zh-TW"]);
  const runs = langSpans(spans);
  assert.deepEqual(runTexts(spans, runs), ["onetwo", "三"]);
});

test("a run absorbs the separator gap between two of its own nodes", () => {
  // collectVisibleText writes a newline between block-level nodes, which is a
  // gap in the byte offsets rather than a span of its own.
  const runs = langSpans([
    { byteStart: 0, byteEnd: 3, lang: "en" },
    { byteStart: 4, byteEnd: 7, lang: "en" },
  ]);
  assert.deepEqual(runs, [{ start: 0, end: 7, lang: "en" }]);
});

test("a run with no declaration breaks the fold and is not reported", () => {
  const spans = spansOf(["one", "en"], ["gap", null], ["two", "en"]);
  const runs = langSpans(spans);
  assert.deepEqual(runTexts(spans, runs), ["one", "two"]);
});

test("no lang attributes anywhere reports nothing", () => {
  assert.deepEqual(langSpans(spansOf(["中文", null], ["更多", null])), []);
});

test("an empty lang is reported verbatim, not dropped", () => {
  // HTML reads it as "language unknown", which cancels an outer declaration.
  // Deciding that is the scanner's job, so it has to reach the scanner.
  const spans = spansOf(["中文", ""]);
  assert.deepEqual(
    langSpans(spans).map((run) => run.lang),
    [""],
  );
});
