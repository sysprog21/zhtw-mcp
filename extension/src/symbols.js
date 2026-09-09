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

  // Read as character codes.  This is the inner loop of both the page scan and
  // the per-field check, and indexing a string allocates a one-character string
  // for every character it walks past.  Only ASCII reaches the second test, so
  // it covers the whitespace \s does: tab through carriage return, and space.
  function isAlphanumericOrSpace(code) {
    return (
      (code >= 0x30 && code <= 0x39) ||
      (code >= 0x41 && code <= 0x5a) ||
      (code >= 0x61 && code <= 0x7a) ||
      code === 0x20 ||
      (code >= 0x09 && code <= 0x0d)
    );
  }

  function repeatedAsciiSymbolRuns(value) {
    const runs = [];
    let index = 0;
    while (index < value.length) {
      const code = value.charCodeAt(index);
      if (code > 0x7f || isAlphanumericOrSpace(code)) {
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

  // A functional run only counts as markup when it stands alone on its line.
  // A fence or a thematic break occupies a line of its own, while the same
  // characters inside a sentence, as in 訂單---取消, are repeated punctuation.
  function removableSymbolRuns(value) {
    return repeatedAsciiSymbolRuns(value).filter(
      (run) => !(isFunctionalSymbolRun(run) && isAloneOnItsLine(value, run)),
    );
  }

  function isAloneOnItsLine(value, run) {
    return (
      !value.slice(lineStartBefore(value, run.start), run.start).trim() &&
      !value.slice(run.end, lineEndAfter(value, run.end)).trim()
    );
  }

  // Both scan to the nearest break rather than over the whole value.  The page
  // side asks this of the entire flattened text once per candidate, where
  // lastIndexOf for a carriage return that DOM text never contains read every
  // preceding character before returning nothing, and slicing the remainder to
  // search it forward copied the rest of the document for every candidate.
  function lineStartBefore(value, index) {
    for (let at = index - 1; at >= 0; at -= 1) {
      const code = value.charCodeAt(at);
      if (code === 0x0a || code === 0x0d) {
        return at + 1;
      }
    }
    return 0;
  }

  function lineEndAfter(value, index) {
    for (let at = index; at < value.length; at += 1) {
      const code = value.charCodeAt(at);
      if (code === 0x0a || code === 0x0d) {
        return at;
      }
    }
    return value.length;
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
