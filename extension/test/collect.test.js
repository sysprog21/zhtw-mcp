import test from "node:test";
import assert from "node:assert/strict";

// content.js is injected as a classic script and reads the DOM through bare
// globals, so the globals have to exist before it is loaded.  The stub below
// is the smallest thing that walk satisfies: the point is to prove that the
// byte offsets and the lang runs content.js reports index the very string it
// hands to the scanner, which no test over synthetic spans can show.
import "../src/shared.js";

const { utf8ByteLength } = globalThis.ZhtwExtensionShared;

// A minimal DOM

class FakeText {
  constructor(value) {
    this.nodeValue = value;
    this.parentElement = null;
  }
}

class FakeElement {
  constructor(tag, attrs, children) {
    this.tagName = tag.toUpperCase();
    this.attrs = attrs || {};
    this.children = children || [];
    this.parentElement = null;
    this.hidden = false;
    for (const child of this.children) {
      child.parentElement = this;
    }
  }

  getAttribute(name) {
    return Object.hasOwn(this.attrs, name) ? this.attrs[name] : null;
  }

  // Only the two selector shapes content.js uses: a comma-separated list of
  // tag names, and a bare attribute test.
  matches(part) {
    const attr = /^\[([^\]=]+)\]$/.exec(part);
    return attr
      ? Object.hasOwn(this.attrs, attr[1])
      : this.tagName === part.toUpperCase();
  }

  closest(selector) {
    const parts = selector.split(",").map((part) => part.trim()).filter(Boolean);
    for (let node = this; node; node = node.parentElement) {
      if (parts.some((part) => node.matches(part))) {
        return node;
      }
    }
    return null;
  }
}

const el = (tag, attrs, ...children) => new FakeElement(tag, attrs, children);
const txt = (value) => new FakeText(value);

/// Install the globals content.js reads, rooted at the given body element.
function installDom(body) {
  globalThis.window = globalThis;
  globalThis.NodeFilter = { SHOW_TEXT: 4, FILTER_ACCEPT: 1, FILTER_REJECT: 2 };
  globalThis.getComputedStyle = () => ({
    display: "block",
    visibility: "visible",
    opacity: "1",
  });
  globalThis.document = {
    body,
    createTreeWalker(root, _whatToShow, filter) {
      const texts = [];
      (function collect(node) {
        if (node instanceof FakeText) {
          texts.push(node);
          return;
        }
        for (const child of node.children) {
          collect(child);
        }
      })(root);
      const accepted = texts.filter(
        (node) => filter.acceptNode(node) === globalThis.NodeFilter.FILTER_ACCEPT,
      );
      let index = -1;
      return {
        currentNode: null,
        nextNode() {
          index += 1;
          this.currentNode = accepted[index] || null;
          return Boolean(this.currentNode);
        },
      };
    },
    querySelectorAll: () => [],
  };
}

/// Load content.js against the given body and return what COLLECT_TEXT sends.
async function collect(body) {
  installDom(body);
  let listener;
  globalThis.chrome = {
    runtime: { onMessage: { addListener: (fn) => { listener = fn; } } },
  };
  // content.js guards against a second injection, and the module cache would
  // return the same evaluated module anyway, so each test gets a fresh import.
  delete globalThis.__zhtwMcpContentLoaded;
  await import(`../src/content.js?case=${Math.random()}`);

  let response;
  listener({ type: "COLLECT_TEXT" }, null, (value) => {
    response = value;
  });
  return response;
}

/// What each reported run names, decoded out of the collected text.  This is
/// the assertion the whole file exists for: the offsets content.js reports have
/// to index the very string it hands to the scanner.
function langTexts(collected) {
  const bytes = Buffer.from(collected.text, "utf8");
  return collected.lang_spans.map((run) =>
    bytes.subarray(run.start, run.end).toString("utf8"),
  );
}

test("collected runs name the same bytes in the collected text", async () => {
  const body = el(
    "body",
    {},
    el(
      "p",
      { lang: "zh-TW" },
      txt("他說"),
      el("span", { lang: "en" }, txt("I agree, 但")),
      txt("結束。"),
    ),
    el("p", { lang: "ja" }, txt("これは日本語, です")),
    el("p", {}, txt("最後一段，沒有標記")),
  );

  const collected = await collect(body);
  assert.equal(collected.ok, true);
  assert.equal(collected.node_count, 5);
  assert.equal(
    collected.text,
    "他說I agree, 但結束。\nこれは日本語, です\n最後一段，沒有標記",
  );

  assert.deepEqual(langTexts(collected), ["他說", "I agree, 但", "結束。", "これは日本語, です"]);
  assert.deepEqual(
    collected.lang_spans.map((run) => run.lang),
    ["zh-TW", "en", "zh-TW", "ja"],
  );

  // And the flattened string really is what the byte offsets index: multibyte
  // runs and the inserted block separator both shift every later offset.
  assert.equal(collected.lang_spans[1].start, utf8ByteLength("他說"));
});

test("a page with no lang attributes reports no runs", async () => {
  const body = el("body", {}, el("p", {}, txt("全部都要檢查, 對吧")));
  const collected = await collect(body);
  assert.deepEqual(collected.lang_spans, []);
  assert.equal(collected.text, "全部都要檢查, 對吧");
});

test("an inner declaration is reported as its own run", async () => {
  const body = el(
    "body",
    {},
    el(
      "div",
      { lang: "en" },
      txt("English, here"),
      el("span", { lang: "zh-TW" }, txt("中文, 這裡")),
    ),
  );
  const collected = await collect(body);
  assert.deepEqual(langTexts(collected), ["English, here", "中文, 這裡"]);
  assert.deepEqual(
    collected.lang_spans.map((run) => run.lang),
    ["en", "zh-TW"],
  );
});

test("an empty lang is reported rather than inherited", async () => {
  const body = el(
    "body",
    {},
    el(
      "div",
      { lang: "en" },
      txt("English, here"),
      el("span", { lang: "" }, txt("中文, 這裡")),
    ),
  );
  const collected = await collect(body);
  assert.deepEqual(langTexts(collected), ["English, here", "中文, 這裡"]);
  assert.deepEqual(
    collected.lang_spans.map((run) => run.lang),
    ["en", ""],
  );
});
