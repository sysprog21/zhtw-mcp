import test from "node:test";
import assert from "node:assert/strict";

import "../src/symbols.js";

const {
  changedRange,
  isAloneOnItsLine,
  isFunctionalSymbolRun,
  removableSymbolRuns,
  repeatedAsciiSymbolRuns,
  runsTheEditProduced,
} = globalThis.ZhtwExtensionSymbols;

test("a run standing alone on its line is markup, whatever the symbol", () => {
  for (const marker of ["###", ":::", "---", "```", "~~~", "***", "___", "===", "+++"]) {
    assert.deepEqual(removableSymbolRuns(marker), [], marker);
    assert.deepEqual(removableSymbolRuns(`前言\n${marker}\n後語`), [], marker);
  }
});

test("repeated punctuation is removable even when it stands alone", () => {
  for (const run of ["...", "!!!", "???", "<<<", ",,,"]) {
    assert.deepEqual(removableSymbolRuns(run), [{ start: 0, end: 3, value: run }], run);
  }
});

// Markup opens a line and what follows it is content, so the run survives with
// text after it.  The same characters with prose in front are punctuation.
test("a functional symbol is exempt where it opens the line", () => {
  for (const line of ["### Heading", "```rust", "::: note", "--- ", "  ---  "]) {
    assert.deepEqual(removableSymbolRuns(line), [], line);
  }
  assert.deepEqual(removableSymbolRuns("訂單---取消"), [
    { start: 2, end: 5, value: "---" },
  ]);
  assert.deepEqual(removableSymbolRuns("前言\n### 標題"), []);
});

// Three of a character nobody can see are not punctuation to report, and
// deleting them would be deleting something the user cannot even point at.
test("repeated control characters are not a symbol run", () => {
  for (const code of [0x00, 0x01, 0x1f, 0x7f]) {
    const value = "a" + String.fromCharCode(code, code, code) + "b";
    assert.deepEqual(repeatedAsciiSymbolRuns(value), [], "code " + code);
  }
  // The printable neighbours of that range still are.
  assert.deepEqual(repeatedAsciiSymbolRuns("a!!!b"), [
    { start: 1, end: 4, value: "!!!" },
  ]);
  assert.deepEqual(repeatedAsciiSymbolRuns("a~~~b"), [
    { start: 1, end: 4, value: "~~~" },
  ]);
});

test("editable symbol handling identifies repeated nonfunctional symbols", () => {
  assert.deepEqual(removableSymbolRuns("請確認...再送出!!!"), [
    { start: 3, end: 6, value: "..." },
    { start: 9, end: 12, value: "!!!" },
  ]);
});

// What content.js composes to find the candidates it offers the page scan.
test("a run is standalone only when nothing else shares its line", () => {
  const standalone = (value) =>
    repeatedAsciiSymbolRuns(value).filter((run) => isAloneOnItsLine(value, run));

  assert.deepEqual(standalone("###"), [{ start: 0, end: 3, value: "###" }]);
  assert.deepEqual(standalone("  ###  "), [{ start: 2, end: 5, value: "###" }]);
  assert.deepEqual(standalone("前\n###\n後"), [{ start: 2, end: 5, value: "###" }]);
  assert.deepEqual(standalone("標題 ###"), []);
  assert.deepEqual(standalone("段落:::註記"), []);
});

test("the functional set covers Markdown, not punctuation", () => {
  for (const value of ["###", ":::", "---", "```", "***", ">>>", "|||"]) {
    assert.equal(isFunctionalSymbolRun({ value }), true, value);
  }
  for (const value of ["...", "!!!", "???", "///", '"""']) {
    assert.equal(isFunctionalSymbolRun({ value }), false, value);
  }
});

// What keeps a keystroke from deleting a run the user typed somewhere else.
test("changedRange names only the span an edit touched", () => {
  assert.deepEqual(changedRange("Note...", "ANote..."), { start: 0, end: 1 });
  assert.deepEqual(changedRange("Wait..", "Wait..."), { start: 6, end: 7 });
  assert.deepEqual(changedRange("abc", "abc"), { start: 3, end: 3 });
  assert.deepEqual(changedRange("", "他說..."), { start: 0, end: 5 });
});

test("only the runs an edit reached are offered for removal", () => {
  const value = "ANote...";
  const runs = removableSymbolRuns(value);

  assert.deepEqual(runs, [{ start: 5, end: 8, value: "..." }]);
  // Typing "A" at the head leaves the distant ellipsis alone.
  assert.deepEqual(runsTheEditProduced(runs, changedRange("Note...", value)), []);
  // Completing the run puts the caret at its end, which counts as touching.
  assert.deepEqual(runsTheEditProduced(runs, changedRange("ANote..", value)), runs);
});

test("a run beside an unrelated edit is not offered for removal", () => {
  const before = removableSymbolRuns("A...");
  const after = removableSymbolRuns("...A");

  assert.deepEqual(runsTheEditProduced(before, changedRange("...", "A...")), []);
  assert.deepEqual(runsTheEditProduced(after, changedRange("...", "...A")), []);
});

test("line-alone detection reads the text around the run", () => {
  const alone = (value, start, end) => isAloneOnItsLine(value, { start, end });

  assert.equal(alone("---", 0, 3), true);
  assert.equal(alone("前\n---\n後", 2, 5), true);
  assert.equal(alone("  ---  ", 2, 5), true);
  assert.equal(alone("foo---bar", 3, 6), false);
  assert.equal(alone("段落:::註記", 2, 5), false);
});

// Reaching into a run is not producing it.  Taking the edit's characters back
// out has to leave less than a run, or it was already there.
test("only a run the edit produced is offered for removal", () => {
  const produced = (previous, next) =>
    runsTheEditProduced(removableSymbolRuns(next), changedRange(previous, next));

  // The third character completes it, so the whole run goes.
  assert.deepEqual(produced("請確認!!", "請確認!!!"), [
    { start: 3, end: 6, value: "!!!" },
  ]);
  // A fourth typed into a run that was already there takes nothing.
  assert.deepEqual(produced("...", "...."), []);
  assert.deepEqual(produced("a...b", "a....b"), []);
  // Neither end was a run of its own, so joining them produced this one.  The
  // two ends have to be measured apart: added together they are three.
  assert.deepEqual(produced("..X..", "....."), [
    { start: 0, end: 5, value: "....." },
  ]);
  // Pasting one in whole is still this edit's doing.
  assert.deepEqual(produced("", "..."), [{ start: 0, end: 3, value: "..." }]);
});

// Falls out of the same rule: a deletion adds no characters, so it can reveal
// a run but never produce one.
test("a run a deletion merely revealed is left alone", () => {
  const produced = (previous, next) =>
    runsTheEditProduced(removableSymbolRuns(next), changedRange(previous, next));

  assert.deepEqual(produced("!!x!", "!!!"), []);
  assert.deepEqual(produced("a...b", "a..."), []);
});
