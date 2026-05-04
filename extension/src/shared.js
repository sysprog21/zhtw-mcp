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
    formatBadgeText,
    normalizeIssue,
    utf8ByteLength,
  };
});
