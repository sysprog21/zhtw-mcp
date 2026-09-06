// chrome.scripting.executeScript injects classic scripts, so this file cannot
// use ESM exports and publishes onto the global instead.  content.js reads it
// from `window`; the tests import this file and read the same global, so they
// exercise the same seam the browser does.
(function initShared(root, factory) {
  root.ZhtwExtensionShared = factory();
})(typeof globalThis !== "undefined" ? globalThis : self, function buildShared() {
  const encoder =
    typeof TextEncoder !== "undefined" ? new TextEncoder() : undefined;

  function utf8ByteLength(text) {
    if (!text) {
      return 0;
    }
    if (encoder) {
      return encoder.encode(text).length;
    }
    return Buffer.byteLength(text, "utf8");
  }

  function byteOffsetToCodeUnit(text, byteOffset) {
    if (byteOffset <= 0) {
      return 0;
    }

    let bytes = 0;
    let codeUnits = 0;
    for (const char of text) {
      const next = bytes + utf8ByteLength(char);
      if (next > byteOffset) {
        return codeUnits;
      }
      bytes = next;
      codeUnits += char.length;
      if (bytes === byteOffset) {
        return codeUnits;
      }
    }
    return text.length;
  }

  function normalizeIssue(issue) {
    return {
      offset: Number(issue.offset) || 0,
      length: Number(issue.length) || 0,
      found: issue.found || "",
      suggestions: Array.isArray(issue.suggestions) ? issue.suggestions : [],
      rule_type: issue.rule_type || "unknown",
      severity: issue.severity || "info",
      context: issue.context || "",
      english: issue.english || "",
    };
  }

  // The scanner reports UTF-8 byte offsets into one concatenated string; the
  // DOM wants code-unit offsets into individual text nodes.  An issue can also
  // straddle several nodes, so one issue becomes zero or more segments.
  //
  // Kept here rather than in content.js because this is the only arithmetic in
  // the extension that can be wrong in a way nobody sees: a highlight landing
  // one character off still looks like a highlight.  Everything it needs is a
  // plain number or string, so it is testable without a DOM.
  //
  // `spans` are `{ byteStart, byteEnd, text }` in document order.  Each
  // returned segment is `{ index, start, end }`, code units into
  // `spans[index].text`, and `end` is always greater than `start`.
  function issueSegments(spans, issue) {
    if (!issue.length) {
      return [];
    }
    const endByte = issue.offset + issue.length;
    const startIndex = spans.findIndex(
      (span) => issue.offset >= span.byteStart && issue.offset < span.byteEnd,
    );
    const endIndex = spans.findIndex(
      (span) => endByte > span.byteStart && endByte <= span.byteEnd,
    );
    if (startIndex < 0 || endIndex < startIndex) {
      return [];
    }

    const segments = [];
    for (let index = startIndex; index <= endIndex; index += 1) {
      const span = spans[index];
      const segmentStartByte = index === startIndex ? issue.offset : span.byteStart;
      const segmentEndByte = index === endIndex ? endByte : span.byteEnd;
      if (segmentStartByte >= segmentEndByte) {
        continue;
      }

      const text = span.text || "";
      const start = byteOffsetToCodeUnit(text, segmentStartByte - span.byteStart);
      const end = byteOffsetToCodeUnit(text, segmentEndByte - span.byteStart);
      if (start >= end) {
        continue;
      }
      segments.push({ index, start, end });
    }
    return segments;
  }

  // The runs of the flattened text that carry a declared language, ready for
  // the scanner to decide which of them it should leave alone.
  //
  // No judgement is made here about which languages count as Chinese: that
  // lives in src/engine/html_lang.rs, on the other side of the wasm call, so
  // there is one definition of it rather than one per side.  What this does is
  // read each span's inherited lang and fold consecutive spans that declared
  // the same one, which keeps a page whose html element carries a lang from
  // sending one entry per text node.
  //
  // Here rather than in content.js for the same reason issueSegments is: this
  // is byte arithmetic over the flattened string, and an off-by-one silences
  // the wrong run without anything looking broken.  The spans are the
  // byteStart, byteEnd and lang records collectVisibleText builds, in document
  // order.  A run absorbs the separator bytes between its own nodes, which sit
  // under the same declaration, but never a span that declared something else
  // or nothing.
  function langSpans(spans) {
    const runs = [];
    let previous = null;
    for (const span of spans) {
      if (typeof span.lang !== "string") {
        // An undeclared span breaks the run rather than being folded through:
        // extending across it would claim bytes no declaration covers.
        previous = null;
        continue;
      }
      // Folding across the gap a block separator left is right, because that
      // gap sits between two nodes under the same declaration.
      if (previous && previous.lang === span.lang) {
        runs[runs.length - 1].end = span.byteEnd;
      } else {
        runs.push({ start: span.byteStart, end: span.byteEnd, lang: span.lang });
      }
      previous = span;
    }
    return runs;
  }

  function tooltipForIssue(issue) {
    const suggestion = issue.suggestions.length
      ? `建議：${issue.suggestions.join("、")}`
      : "無自動建議";
    const context = issue.context ? `\n說明：${issue.context}` : "";
    const english = issue.english ? `\nEnglish：${issue.english}` : "";
    return `${issue.found} — ${issue.rule_type} / ${issue.severity}\n${suggestion}${context}${english}`;
  }

  return {
    byteOffsetToCodeUnit,
    issueSegments,
    langSpans,
    normalizeIssue,
    tooltipForIssue,
    utf8ByteLength,
  };
});
