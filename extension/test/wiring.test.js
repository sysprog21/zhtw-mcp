import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import * as format from "../src/format.js";
import "../src/shared.js";
import "../src/symbols.js";
import "../src/editable.js";

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

// Both are injected as classic scripts, so a destructure is a plain property
// read at runtime: a missing name is `undefined` and fails on first call, not
// at load.
const GLOBAL_MODULES = [
  { global: "ZhtwExtensionShared", file: "shared.js" },
  { global: "ZhtwExtensionSymbols", file: "symbols.js" },
  { global: "ZhtwExtensionEditable", file: "editable.js" },
];

for (const { global, file } of GLOBAL_MODULES) {
  test(`every name content.js destructures from ${global} exists`, () => {
    const names = destructuredFrom(src("content.js"), `window.${global}`);

    assert.ok(names.length > 0, `content.js should destructure ${global}`);
    for (const name of names) {
      assert.equal(
        typeof globalThis[global][name],
        "function",
        `${file} does not export ${name}, but content.js destructures it`,
      );
    }
  });

  // Every classic script has to be injected, and executeScript takes them in
  // order: a file added here and not there is undefined at content.js load.
  test(`background.js injects ${file}`, () => {
    assert.match(src("background.js"), new RegExp(`"src/${quote(file)}"`));
  });
}

test("the injected globals do not define the same helper twice", () => {
  // The set drifted before: shared.js owned badge helpers that only its test
  // used, while background.js carried live copies of the same rules.
  const surfaces = [
    ["format.js", format],
    ...GLOBAL_MODULES.map(({ global, file }) => [file, globalThis[global]]),
  ];

  for (const [nameA, a] of surfaces) {
    for (const [nameB, b] of surfaces) {
      if (nameA >= nameB) {
        continue;
      }
      // Own properties only: a helper named toString or valueOf would
      // otherwise look like it collided with every other module.
      const overlap = Object.keys(a).filter((key) => Object.hasOwn(b, key));
      assert.deepEqual(overlap, [], `defined in both ${nameA} and ${nameB}: ${overlap}`);
    }
  }
});

// The lang payload crosses into Rust through serde, which ignores a field the
// struct does not declare unless the struct opts into #[serde(deny_unknown_fields)],
// and ScanOptions does not.  Rename or drop lang_spans on the Rust side and this
// side goes on sending it: not an error, just silently absent, and the extension
// stops honoring lang with nothing in any console to say so.  (The container's
// #[serde(default)] is the other half, and a different one: it fills in a field
// the payload omits.)  Reading the struct is the only way this side can notice.
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

// The popup is the only place a user can name a rule family, and the names are
// decided in Rust: scan_text deserializes the off array into RuleFamily through
// serde, which rejects an unknown variant, so a family renamed there and not
// here fails the scan.  The CLI and the MCP tool reach the same names through
// from_str_strict instead, and a unit test in src/config.rs pins those two
// spellings to each other.  Reading the enum is how this side notices.
const rulesetSource = readFileSync(
  fileURLToPath(new URL("../../src/rules/ruleset.rs", import.meta.url)),
  "utf8",
);

/// The family names RuleFamily::name() returns, in declaration order.
function ruleFamilyNames(source) {
  // src/rules/ruleset.rs defines name() on six enums, so the lookup has to
  // start at the impl block rather than at the first match in the file.
  const impl = source.indexOf("impl RuleFamily {");
  assert.ok(impl !== -1, "impl RuleFamily not found in src/rules/ruleset.rs");
  const body = /fn name\(self\) -> &'static str \{([\s\S]*?)\n    \}/.exec(
    source.slice(impl),
  );
  assert.ok(body, "RuleFamily::name not found in src/rules/ruleset.rs");
  return [...body[1].matchAll(/=>\s*"([a-z_]+)"/g)].map((match) => match[1]);
}

/// The values of the option elements inside one select in popup.html.
function optionValuesOf(html, selectId) {
  const block = new RegExp(
    `<select id="${quote(selectId)}"[^>]*>([\\s\\S]*?)</select>`,
  ).exec(html);
  assert.ok(block, `select #${selectId} not found in popup.html`);
  return [...block[1].matchAll(/value="([^"]+)"/g)].map((match) => match[1]);
}

/// The popup.html source, read once for every contract test below.
const popupHtml = readFileSync(
  fileURLToPath(new URL("../popup.html", import.meta.url)),
  "utf8",
);

/// The variant names of a Rust enum, lower cased.
///
/// ScanOptions deserializes SpacingPolicy under #[serde(rename_all =
/// "snake_case")], and every variant is one word, so the wire spelling is the
/// variant name in lower case.
function enumVariantNames(source, enumName) {
  const body = new RegExp(`enum ${quote(enumName)} \\{([\\s\\S]*?)\\n\\}`).exec(source);
  assert.ok(body, `enum ${enumName} not found in src/rules/ruleset.rs`);
  return [...body[1].matchAll(/^\s*([A-Z][A-Za-z]*),/gm)].map((match) =>
    match[1].toLowerCase(),
  );
}

// Each select reaches the scanner through serde, which rejects an unknown
// variant: a name renamed in Rust and not here fails the whole scan rather
// than falling back to a default. Reading the Rust source is how this side
// notices.
const SELECT_CONTRACTS = [
  {
    select: "off",
    field: "off",
    names: () => ruleFamilyNames(rulesetSource),
    senders: ["popup.js", "background.js"],
    rustName: "RuleFamily",
  },
  {
    select: "spacing",
    field: "spacing",
    names: () => enumVariantNames(rulesetSource, "SpacingPolicy"),
    senders: ["popup.js", "background.js"],
    rustName: "SpacingPolicy",
  },
];

for (const { select, field, names, senders, rustName } of SELECT_CONTRACTS) {
  test(`the popup offers exactly the ${select} values the scanner accepts`, () => {
    assert.deepEqual(
      optionValuesOf(popupHtml, select),
      names(),
      `popup.html and ${rustName} disagree about the names`,
    );

    assert.ok(
      rustFieldsOf(wasmSource, "ScanOptions").includes(field),
      `ScanOptions no longer has a ${field} field`,
    );
    for (const file of senders) {
      assert.match(
        src(file),
        new RegExp(`${quote(field)}\\s*:`),
        `${file} does not send ${field}`,
      );
    }
  });
}

test("the structural-symbol exclusions cross from content collection to WASM", () => {
  assert.match(src("content.js"), /excluded_spans\s*:/);
  assert.match(src("background.js"), /excluded_spans\s*:/);
  assert.match(wasmSource, /excluded_spans\s*:/);
});

test("the symbol handling mode crosses from the popup to the content script", () => {
  assert.match(src("popup.js"), /symbol_handling\s*:/);
  // It rides on COLLECT_TEXT rather than on a message of its own, so a tab
  // still holding an older content script answers the scan instead of leaving
  // the port to close on the sender.
  assert.match(src("background.js"), /type:\s*"COLLECT_TEXT",\s*\n\s*symbol_handling\s*:/);
  assert.match(src("content.js"), /message\.symbol_handling/);
  assert.match(src("editable.js"), /SYMBOL_HANDLING_MODES\.includes/);
  assert.doesNotMatch(src("background.js"), /CONFIGURE_SYMBOL_HANDLING/);
});

// The one select whose values never reach Rust, so the contracts above cannot
// see it.  popup.html offers the modes, popup.js validates what it restores
// from storage, and content.js validates what arrives on the wire; a mode added
// to one and not the others is not an error, it silently becomes "warn".
test("the popup offers exactly the symbol handling modes both sides accept", () => {
  const modesIn = (file) => {
    const match = /const SYMBOL_HANDLING_MODES = \[([^\]]*)\]/.exec(src(file));
    assert.ok(match, `${file} does not declare SYMBOL_HANDLING_MODES`);
    return [...match[1].matchAll(/"([a-z]+)"/g)].map((entry) => entry[1]).sort();
  };

  const offered = optionValuesOf(popupHtml, "symbol-handling").sort();
  assert.deepEqual(offered, modesIn("popup.js"), "popup.html and popup.js disagree");
  assert.deepEqual(offered, modesIn("editable.js"), "popup.html and editable.js disagree");
  // The default the two validators fall back to has to be one of them.
  assert.ok(offered.includes("warn"), "warn is the fallback and must be offered");
});

// editable.js puts its overlay in the page, and collectVisibleText walks the
// page: the marker one writes has to be the one the other skips, or the warning
// text joins the scan and reports itself.
test("the collector skips the overlay editable.js marks", () => {
  const marker = /dataset\.([A-Za-z]+)\s*=/.exec(src("editable.js"));
  assert.ok(marker, "editable.js should mark its overlay with a data attribute");
  const attribute = marker[1].replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
  assert.match(src("content.js"), new RegExp(`\\[data-${attribute}\\]`));
});

// The stored mode arrives asynchronously, so a scan started before the read
// lands would send the visible default instead of what the user chose.
test("the popup waits for the stored mode before it scans", () => {
  assert.match(src("popup.js"), /await symbolHandlingLoad;\s*\n\s*await runScan\(\);/);
  // And the read has to settle rather than reject, or that await throws on
  // every press and the scan never runs at all.
  assert.match(src("popup.js"), /catch \(error\) \{[\s\S]*?\}\s*finally \{/);
});
