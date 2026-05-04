const test = require("node:test");
const assert = require("node:assert/strict");

const {
  byteOffsetToCodeUnit,
  countBadgeIssues,
  formatBadgeText,
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
