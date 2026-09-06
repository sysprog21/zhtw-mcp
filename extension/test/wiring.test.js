import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as format from "../src/format.js";
import "../src/shared.js";

const src = (name) =>
  readFileSync(fileURLToPath(new URL(`../src/${name}`, import.meta.url)), "utf8");

/// Regex-quote a literal so `.` in a path or a property chain stays a dot.
/// Without this, "./format.js" also matches "./formatXjs" and the check passes
/// on exactly the typo it exists to catch.
function quote(literal) {
  return literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/// Names pulled from a `import { a, b } from "<specifier>"` statement.
function namedImportsFrom(source, specifier) {
  const pattern = new RegExp(
    `import\\s*\\{([^}]*)\\}\\s*from\\s*["']${quote(specifier)}["']`,
    "g",
  );
  const names = [];
  for (const match of source.matchAll(pattern)) {
    for (const part of match[1].split(",")) {
      const name = part.trim().split(/\s+as\s+/)[0].trim();
      if (name) {
        names.push(name);
      }
    }
  }
  return names;
}

/// Names pulled out of a `const { a, b } = <object>` destructure.
function destructuredFrom(source, object) {
  const match = new RegExp(
    `const\\s*\\{([^}]*)\\}\\s*=\\s*${quote(object)}`,
  ).exec(source);
  return match
    ? match[1]
        .split(",")
        .map((part) => part.trim())
        .filter(Boolean)
    : [];
}

// Node links ES modules lazily, and background.js cannot be imported at all
// without extension/dist, which only exists after build-wasm.sh has run.  A
// mis-wired import would therefore surface first in a browser, where nobody is
// watching a console.  Checking the names against the real export surface is
// cheap and needs neither wasm nor a DOM.
for (const file of ["background.js", "popup.js"]) {
  test(`every name ${file} imports from format.js is exported`, () => {
    const imported = namedImportsFrom(src(file), "./format.js");

    assert.ok(imported.length > 0, `${file} should import from format.js`);
    for (const name of imported) {
      // Presence, not type: format.js exports MAX_STORED_ISSUES too, and any
      // file is entitled to import it.
      assert.ok(
        name in format,
        `format.js does not export ${name}, but ${file} imports it`,
      );
    }
  });
}

test("every name content.js destructures from the shared global exists", () => {
  // shared.js is injected as a classic script, so this is a plain property
  // read at runtime: a missing name is `undefined` and fails on first call,
  // not at load.
  const names = destructuredFrom(src("content.js"), "window.ZhtwExtensionShared");

  assert.ok(names.length > 0, "content.js should destructure the shared global");
  for (const name of names) {
    assert.equal(
      typeof globalThis.ZhtwExtensionShared[name],
      "function",
      `shared.js does not export ${name}, but content.js destructures it`,
    );
  }
});

test("shared.js and format.js do not both define the same helper", () => {
  // The pair drifted before: shared.js owned badge helpers that only its test
  // used, while background.js carried live copies of the same rules.
  const overlap = Object.keys(globalThis.ZhtwExtensionShared).filter(
    (name) => name in format,
  );

  assert.deepEqual(overlap, [], `defined in both shared.js and format.js: ${overlap}`);
});

// The lang payload crosses into Rust through serde, and ScanOptions carries
// #[serde(default)] at the container level: a field the scanner no longer
// recognizes is not an error, it is silently absent, and the extension quietly
// stops honoring lang with nothing in any console to say so.  Reading the
// struct is the only way this side can notice.
const wasmSource = readFileSync(
  fileURLToPath(new URL("../../src/wasm.rs", import.meta.url)),
  "utf8",
);

/// Field names declared by a Rust struct, ignoring doc comments and attributes.
function rustFieldsOf(source, structName) {
  const body = new RegExp(`struct\\s+${structName}\\s*\\{([^}]*)\\}`).exec(source);
  assert.ok(body, `${structName} not found in src/wasm.rs`);
  return [...body[1].matchAll(/^\s*([a-z_][a-z0-9_]*)\s*:/gm)].map((match) => match[1]);
}

test("the lang payload matches the struct the scanner deserializes it into", () => {
  const optionKey = rustFieldsOf(wasmSource, "ScanOptions").find(
    (name) => name === "lang_spans",
  );
  assert.ok(optionKey, "ScanOptions no longer has a lang_spans field");

  for (const file of ["content.js", "background.js"]) {
    assert.match(
      src(file),
      new RegExp(`${optionKey}\\s*:`),
      `${file} does not send ${optionKey}`,
    );
  }

  const runs = globalThis.ZhtwExtensionShared.langSpans([
    { byteStart: 0, byteEnd: 2, lang: "en" },
  ]);
  assert.equal(runs.length, 1, "langSpans should report a declared run");
  assert.deepEqual(
    Object.keys(runs[0]).sort(),
    rustFieldsOf(wasmSource, "LangSpan").sort(),
    "langSpans emits different fields than LangSpan deserializes",
  );
});
