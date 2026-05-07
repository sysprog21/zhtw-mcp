(function initShared(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }
  root.ZhtwExtensionShared = api;
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

  function countBadgeIssues(issues) {
    return issues.filter(
      (issue) => issue.severity === "warning" || issue.severity === "error",
    ).length;
  }

  const extensionIgnoredPatterns = ["...", ":::"];

  function filterExtensionIgnoredIssues(scanResult, text = "") {
    const issues = Array.isArray(scanResult?.issues) ? scanResult.issues : [];
    const ignoredRanges = ignoredPatternRanges(text);
    const filteredIssues = issues
      .map(normalizeDisplayIssue)
      .filter(
        (issue) =>
          !shouldIgnoreExtensionIssue(issue, ignoredRanges) &&
          !shouldHideExtensionIssue(issue),
      );

    return {
      ...scanResult,
      issues: filteredIssues,
      issue_count: filteredIssues.length,
      badge_count: countBadgeIssues(filteredIssues),
      severity_counts: countSeverityIssues(filteredIssues),
    };
  }

  function shouldIgnoreExtensionIssue(issue, ignoredRanges = []) {
    const found = issue?.found || "";
    if (extensionIgnoredPatterns.includes(found)) {
      return true;
    }

    const offset = Number(issue?.offset);
    const length = Number(issue?.length);
    if (!Number.isFinite(offset) || !Number.isFinite(length) || length <= 0) {
      return false;
    }
    const end = offset + length;

    return ignoredRanges.some(
      (range) => offset < range.byteEnd && end > range.byteStart,
    );
  }

  function shouldHideExtensionIssue(issue) {
    return issue?.severity === "info" && !hasVisibleSuggestion(issue);
  }

  function hasVisibleSuggestion(issue) {
    return Array.isArray(issue?.suggestions)
      ? issue.suggestions.some((suggestion) => String(suggestion).trim())
      : false;
  }

  function normalizeDisplayIssue(issue) {
    return {
      ...issue,
      suggestions: Array.isArray(issue?.suggestions)
        ? issue.suggestions.filter((suggestion) => String(suggestion).trim())
        : [],
    };
  }

  function ignoredPatternRanges(text) {
    if (!text) {
      return [];
    }

    const byteOffsets = codeUnitByteOffsets(text);
    const ranges = [];
    for (const pattern of extensionIgnoredPatterns) {
      let searchStart = 0;
      while (searchStart < text.length) {
        const index = text.indexOf(pattern, searchStart);
        if (index < 0) {
          break;
        }
        const byteStart = byteOffsets[index];
        ranges.push({
          byteStart,
          byteEnd: byteOffsets[index + pattern.length],
        });
        searchStart = index + pattern.length;
      }
    }
    return ranges;
  }

  function codeUnitByteOffsets(text) {
    const offsets = new Array(text.length + 1);
    let bytes = 0;
    let index = 0;
    offsets[0] = 0;

    for (const char of text) {
      bytes += utf8ByteLength(char);
      const nextIndex = index + char.length;
      for (
        let offsetIndex = index + 1;
        offsetIndex <= nextIndex;
        offsetIndex += 1
      ) {
        offsets[offsetIndex] = bytes;
      }
      index = nextIndex;
    }

    return offsets;
  }

  function countSeverityIssues(issues) {
    return issues.reduce(
      (counts, issue) => {
        if (issue.severity === "error") {
          counts.error += 1;
        } else if (issue.severity === "warning") {
          counts.warning += 1;
        } else {
          counts.info += 1;
        }
        return counts;
      },
      { info: 0, warning: 0, error: 0 },
    );
  }

  function formatBadgeText(count) {
    if (!count) {
      return "";
    }
    return count > 99 ? "99+" : String(count);
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

  return {
    byteOffsetToCodeUnit,
    countBadgeIssues,
    filterExtensionIgnoredIssues,
    formatBadgeText,
    normalizeIssue,
    shouldIgnoreExtensionIssue,
    shouldHideExtensionIssue,
    utf8ByteLength,
  };
});
