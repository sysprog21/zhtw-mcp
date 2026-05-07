const test = require("node:test");
const assert = require("node:assert/strict");

const {
  byteOffsetToCodeUnit,
  countBadgeIssues,
  filterExtensionIgnoredIssues,
  formatBadgeText,
  shouldHideExtensionIssue,
  shouldIgnoreExtensionIssue,
  utf8ByteLength,
} = require("../src/shared.js");

test("UTF-8 byte offsets map back to JavaScript code units", () => {
  const text = "A軟件B";
  const start = utf8ByteLength("A");
  const end = start + utf8ByteLength("軟件");

  assert.equal(byteOffsetToCodeUnit(text, start), 1);
  assert.equal(byteOffsetToCodeUnit(text, end), 3);
});

test("badge count includes warnings and errors but not info", () => {
  const issues = [
    { severity: "info" },
    { severity: "warning" },
    { severity: "error" },
  ];

  assert.equal(countBadgeIssues(issues), 2);
});

test("badge text is capped for dense pages", () => {
  assert.equal(formatBadgeText(0), "");
  assert.equal(formatBadgeText(7), "7");
  assert.equal(formatBadgeText(125), "99+");
});

test("extension ignores configured punctuation-only patterns", () => {
  const text = "流程...結束\n::: note\n這個軟件";
  const scanResult = {
    issues: [
      {
        offset: utf8ByteLength("流程"),
        length: utf8ByteLength("..."),
        found: "...",
        severity: "warning",
      },
      {
        offset: utf8ByteLength("流程...結束\n"),
        length: utf8ByteLength(":::"),
        found: ":::",
        severity: "error",
      },
      {
        offset: utf8ByteLength("流程...結束\n::: note\n這個"),
        length: utf8ByteLength("軟件"),
        found: "軟件",
        severity: "warning",
      },
      {
        offset: 0,
        length: utf8ByteLength("流程"),
        found: "流程",
        severity: "info",
      },
    ],
    issue_count: 4,
    badge_count: 3,
    severity_counts: { info: 1, warning: 2, error: 1 },
  };

  const filtered = filterExtensionIgnoredIssues(scanResult, text);

  assert.deepEqual(
    filtered.issues.map((issue) => issue.found),
    ["軟件"],
  );
  assert.equal(filtered.issue_count, 1);
  assert.equal(filtered.badge_count, 1);
  assert.deepEqual(filtered.severity_counts, { info: 0, warning: 1, error: 0 });
});

test("extension ignores scanner issues inside configured pattern ranges", () => {
  const text = "😀提示:::內容";
  const issue = {
    offset: utf8ByteLength("😀提示"),
    length: utf8ByteLength(":"),
    found: ":",
    severity: "warning",
  };

  const filtered = filterExtensionIgnoredIssues({ issues: [issue] }, text);

  assert.equal(shouldIgnoreExtensionIssue({ found: ":::" }), true);
  assert.equal(filtered.issues.length, 0);
});

test("extension hides info issues without visible suggestions", () => {
  const scanResult = {
    issues: [
      {
        offset: 0,
        length: utf8ByteLength("可以說是"),
        found: "可以說是",
        suggestions: [""],
        rule_type: "ai_style",
        severity: "info",
      },
      {
        offset: utf8ByteLength("可以說是中文"),
        length: 0,
        found: "",
        suggestions: [" "],
        rule_type: "punctuation",
        severity: "info",
      },
      {
        offset: 0,
        length: utf8ByteLength("軟件"),
        found: "軟件",
        suggestions: ["軟體"],
        rule_type: "cross_strait",
        severity: "warning",
      },
      {
        offset: 0,
        length: utf8ByteLength("刪"),
        found: "刪",
        suggestions: [""],
        rule_type: "grammar",
        severity: "warning",
      },
    ],
    issue_count: 4,
    badge_count: 2,
    severity_counts: { info: 2, warning: 2, error: 0 },
  };

  const filtered = filterExtensionIgnoredIssues(scanResult, "可以說是中文軟件");

  assert.equal(shouldHideExtensionIssue(scanResult.issues[0]), true);
  assert.deepEqual(
    filtered.issues.map((issue) => issue.found),
    ["軟件", "刪"],
  );
  assert.deepEqual(filtered.issues[1].suggestions, []);
  assert.equal(filtered.issue_count, 2);
  assert.equal(filtered.badge_count, 2);
  assert.deepEqual(filtered.severity_counts, { info: 0, warning: 2, error: 0 });
});
