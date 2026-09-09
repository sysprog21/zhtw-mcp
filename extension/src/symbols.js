// The editable-field half of the extension: what counts as a run of repeated
// symbols, which runs are markup and therefore left alone, and where in a field
// an edit landed.  Kept apart from shared.js, which is about mapping scanner
// byte offsets back onto the page; the two share no state and are read by
// different parts of content.js.
//
// Injected as a classic script like shared.js, so it publishes onto the global
// rather than exporting.
(function initSymbols(root, factory) {
  root.ZhtwExtensionSymbols = factory();
})(typeof globalThis !== "undefined" ? globalThis : self, function buildSymbols() {
  // Characters whose repeated run is markup rather than prose: Markdown
  // headings, thematic breaks, setext underlines, fenced code, TOML front
  // matter, table and quote gutters, and the ::: fence an accessibility anchor
  // uses.  A run of one of these is left alone in an editable field and is
  // taken out of the page scan; anything else, !!! and ??? among them, is the
  // repeated punctuation this feature exists to catch.
  const FUNCTIONAL_SYMBOLS = new Set([
    "#",
    ":",
    "-",
    "~",
    "*",
    "_",
    "=",
    "+",
    "`",
    ">",
    "|",
  ]);

  // Printable ASCII that is not a letter or a digit.  Read as a character code
  // because this is the inner loop of both the page scan and the per-field
  // check, and indexing a string allocates a one-character string for every
  // character it walks past.  Control characters fall outside it deliberately:
  // they are not punctuation anyone can see, so three of them are neither a run
  // to warn about nor a run to delete.
  const WHITESPACE = /\s/;

  function isSymbolCode(code) {
    return (
      code >= 0x21 &&
      code <= 0x7e &&
      !(code >= 0x30 && code <= 0x39) &&
      !(code >= 0x41 && code <= 0x5a) &&
      !(code >= 0x61 && code <= 0x7a)
    );
  }

  function repeatedAsciiSymbolRuns(value) {
    const runs = [];
    let index = 0;
    while (index < value.length) {
      const code = value.charCodeAt(index);
      if (!isSymbolCode(code)) {
        index += 1;
        continue;
      }
      let end = index + 1;
      while (value.charCodeAt(end) === code) {
        end += 1;
      }
      if (end - index >= 3) {
        runs.push({ start: index, end, value: value.slice(index, end) });
      }
      index = end;
    }
    return runs;
  }

  // Every run repeatedAsciiSymbolRuns produces is one character repeated, so
  // the first character decides the whole run.
  function isFunctionalSymbolRun(run) {
    return FUNCTIONAL_SYMBOLS.has(run.value[0]);
  }

  function removableSymbolRuns(value) {
    return repeatedAsciiSymbolRuns(value).filter(
      (run) => !(isFunctionalSymbolRun(run) && startsItsLine(value, run)),
    );
  }

  function isAloneOnItsLine(value, run) {
    return blankBefore(value, run.start) && blankAfter(value, run.end);
  }

  // Markup opens a line.  A heading, a fence carrying a language tag, and a
  // fenced note all start one, and what follows is the content they mark
  // rather than more punctuation, so asking for the whole line would read
  // "### Heading" as three repeated hashes and delete them as they were typed.
  // The same characters inside a sentence, as in 訂單---取消, have prose in
  // front of them and are the repeated punctuation this feature is for.
  function startsItsLine(value, run) {
    return blankBefore(value, run.start);
  }

  // Whitespace all the way to the line break, answered by scanning only as far
  // as the answer.  The page side asks this of the whole flattened text once
  // per candidate, and a field can hold one long line carrying a run every few
  // characters, so reaching the break regardless would cost that line's length
  // for every run on it.
  function blankBefore(value, index) {
    for (let at = index - 1; at >= 0; at -= 1) {
      const code = value.charCodeAt(at);
      if (code === 0x0a || code === 0x0d) {
        return true;
      }
      if (!WHITESPACE.test(value[at])) {
        return false;
      }
    }
    return true;
  }

  function blankAfter(value, index) {
    for (let at = index; at < value.length; at += 1) {
      const code = value.charCodeAt(at);
      if (code === 0x0a || code === 0x0d) {
        return true;
      }
      if (!WHITESPACE.test(value[at])) {
        return false;
      }
    }
    return true;
  }

  // The half-open span of `next` that differs from `previous`.  Removal is
  // confined to it so that a keystroke at the top of a field cannot delete a
  // run the user typed somewhere else long ago and never looked at again.
  function changedRange(previous, next) {
    let start = 0;
    const shortest = Math.min(previous.length, next.length);
    while (start < shortest && previous[start] === next[start]) {
      start += 1;
    }
    let end = next.length;
    let previousEnd = previous.length;
    while (
      end > start &&
      previousEnd > start &&
      previous[previousEnd - 1] === next[end - 1]
    ) {
      end -= 1;
      previousEnd -= 1;
    }
    return { start, end };
  }

  // A run this edit produced, rather than one it merely reached into.  What the
  // edit did not touch is the run's two ends, and a run was already there only
  // if one of those ends was one on its own.  Both have to be measured
  // separately: their lengths added together are the length of a run the edit
  // may well have just joined, as typing a dot between two pairs of them does.
  // Typing the third character of a run leaves ends of two and nothing, so the
  // run the edit completed is still removed.
  //
  // An edit that inserted nothing produces nothing, whatever it uncovered:
  // backspacing the x out of !!x! reveals !!! rather than making it, and the
  // three were there to be revealed.  That errs the safe way, which is the only
  // direction worth erring in when the cost is the user's own text.
  function runsTheEditProduced(runs, range) {
    if (range.end === range.start) {
      return [];
    }
    return runsTouching(runs, range).filter(
      (run) =>
        Math.max(0, range.start - run.start) < 3 && Math.max(0, run.end - range.end) < 3,
    );
  }

  // A completed run overlaps the inserted character: typing the third dot
  // changes the span containing that dot.  Boundary contact alone is not
  // enough, because an unrelated insertion may sit beside punctuation the
  // field already contained.
  function runsTouching(runs, range) {
    return runs.filter((run) => run.end > range.start && run.start < range.end);
  }

  return {
    changedRange,
    isAloneOnItsLine,
    isFunctionalSymbolRun,
    removableSymbolRuns,
    repeatedAsciiSymbolRuns,
    runsTheEditProduced,
  };
});
