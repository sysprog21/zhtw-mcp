import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { chromium } from "@playwright/test";

const srcDir = path.resolve(import.meta.dirname, "..", "src");

// These scripts are injected into the page's own world, where the extension
// injects them into an isolated one.  Everything asserted below goes through
// the DOM, which both worlds share: event dispatch, execCommand, node
// insertion, TreeWalker.  The one place the worlds differ is a value setter the
// page patched onto a field, which an isolated world cannot see at all, so the
// framework test is the harder of the two cases rather than the real one.

const PAGE = `<!doctype html><meta charset="utf-8"><body>
  <label>native <textarea id="native"></textarea></label>
  <input id="line" type="text" />
  <input id="tel" type="tel" />
  <input id="url" type="url" />
  <input id="guarded" type="text" readonly value="請等等..." />
  <div id="host" contenteditable="true"><p id="para">先寫點字</p>
    <p id="second">另一段..</p>
    <p id="third">尾段</p>
    <div id="void" contenteditable="false">不可編輯...</div>
  </div>
</body>`;

/// Load shared.js and content.js into the page the way the extension does, and
/// put the content script into `mode` through the message it really listens on.
async function installContentScript(page, mode) {
  await page.setContent(PAGE);
  // content.js reads chrome.runtime at load, so the stub has to be in place
  // before the script tag is added.
  await page.evaluate(() => {
    window.chrome = {
      runtime: {
        onMessage: {
          addListener: (fn) => {
            window.__zhtwListener = fn;
          },
        },
      },
    };
  });
  await page.addScriptTag({ path: path.join(srcDir, "shared.js") });
  await page.addScriptTag({ path: path.join(srcDir, "symbols.js") });
  await page.addScriptTag({ path: path.join(srcDir, "editable.js") });
  await page.addScriptTag({ path: path.join(srcDir, "content.js") });
  await page.evaluate((symbolHandling) => {
    window.__zhtwListener({ type: "COLLECT_TEXT", symbol_handling: symbolHandling }, null, () => {});
  }, mode);
}

async function withPage(mode, body) {
  const profile = await mkdtemp(path.join(tmpdir(), "zhtw-symbols-"));
  const context = await chromium.launchPersistentContext(profile, {
    channel: "chromium",
    headless: true,
  });
  try {
    const page = await context.newPage();
    await installContentScript(page, mode);
    await body(page);
  } finally {
    await context.close();
    await rm(profile, { recursive: true, force: true });
  }
}

test("remove mode deletes the run the edit completed", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    await page.locator("#native").fill("");
    await page.locator("#native").click();
    await page.keyboard.type("請確認!!");
    assert.equal(await page.locator("#native").inputValue(), "請確認!!");
    await page.keyboard.type("!");
    assert.equal(await page.locator("#native").inputValue(), "請確認");
  });
});

// The bug this guards: scanning the whole field on every keystroke deleted text
// the user typed long ago and was not looking at, with no way to undo it.
test("remove mode leaves a run the edit never reached", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    // Set without an input event, the way a page arrives with the field
    // already filled in.  Typing it would be a paste, which remove mode is
    // entitled to clean up.
    await page.evaluate(() => {
      document.querySelector("#native").value = "第一段。\n\n這件事很重要...\n\n第二段。";
    });
    await field.click();
    await page.keyboard.press("Control+Home");
    await page.keyboard.type("A");

    const value = await field.inputValue();
    assert.ok(value.startsWith("A第一段。"), value);
    assert.ok(value.includes("這件事很重要..."), value);
  });
});

test("remove mode leaves a pre-existing run beside an edit", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await page.evaluate(() => {
      const field = document.querySelector("#native");
      field.value = "...";
      field.focus();
      field.setSelectionRange(0, 0);
    });
    await page.keyboard.type("A");
    assert.equal(await field.inputValue(), "A...");
  });

  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await page.evaluate(() => {
      const field = document.querySelector("#native");
      field.value = "...";
      field.focus();
      field.setSelectionRange(3, 3);
    });
    await page.keyboard.type("A");
    assert.equal(await field.inputValue(), "...A");
  });
});

test("removal is undoable, so it never costs the user text outright", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await field.click();
    await page.keyboard.type("嗯...");
    assert.equal(await field.inputValue(), "嗯");
    await page.keyboard.press("Control+z");
    assert.equal(await field.inputValue(), "嗯...");
  });
});

// A framework tracks the field's value through a setter it patches onto the
// element.  Writing straight to `value` from the same world updates that
// tracker, and the framework then concludes nothing changed.  An isolated world
// never sees the patch, so the extension is not exposed to this; the test pins
// the behaviour anyway, because it is what makes the fallback path safe.
test("remove mode reaches a listener that tracks the value like React does", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    await page.evaluate(() => {
      const field = document.querySelector("#line");
      const native = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      );
      let tracked = field.value;
      Object.defineProperty(field, "value", {
        configurable: true,
        get: () => native.get.call(field),
        set(next) {
          tracked = next;
          native.set.call(field, next);
        },
      });
      window.__changes = [];
      field.addEventListener("input", () => {
        const current = native.get.call(field);
        if (current !== tracked) {
          tracked = current;
          window.__changes.push(current);
        }
      });
    });

    await page.locator("#line").click();
    await page.keyboard.type("a...");

    assert.equal(await page.locator("#line").inputValue(), "a");
    assert.deepEqual(await page.evaluate(() => window.__changes.at(-1)), "a");
  });
});

test("a markup fence on its own line survives remove mode", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await field.click();
    await page.keyboard.type("前言\n---\n後語");
    assert.equal(await field.inputValue(), "前言\n---\n後語");
  });
});

test("remove mode edits a contenteditable through the caret's text node", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    await page.locator("#para").click();
    await page.keyboard.press("End");
    await page.keyboard.type("!!!");
    assert.equal(await page.locator("#para").textContent(), "先寫點字");
  });
});

// The warning used to be inserted next to the field, which made it a flex item
// in a flex container, invalid content inside a label, and part of the document
// inside a contenteditable.
test("warn mode puts one overlay on the body, never beside the field", { timeout: 60_000 }, async () => {
  await withPage("warn", async (page) => {
    await page.locator("#native").click();
    await page.keyboard.type("真的嗎???");
    // The check runs on a pause rather than on each key, so wait for it.
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "attached" });

    const placement = await page.evaluate(() => {
      const warnings = [...document.querySelectorAll(".zhtw-mcp-symbol-warning")];
      return {
        count: warnings.length,
        onBody: warnings.every((node) => node.parentElement === document.body),
        insideLabel: Boolean(document.querySelector("label .zhtw-mcp-symbol-warning")),
        insideEditor: Boolean(document.querySelector("#host .zhtw-mcp-symbol-warning")),
      };
    });

    assert.deepEqual(placement, {
      count: 1,
      onBody: true,
      insideLabel: false,
      insideEditor: false,
    });
  });
});

test("warn mode is silent once the field holds no repeated symbol", { timeout: 60_000 }, async () => {
  await withPage("warn", async (page) => {
    await page.locator("#native").click();
    await page.keyboard.type("哦...");
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "attached" });
    await page.keyboard.press("Backspace");
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "detached" });
  });
});

test("off mode touches nothing", { timeout: 60_000 }, async () => {
  await withPage("off", async (page) => {
    await page.locator("#native").click();
    await page.keyboard.type("真的嗎???");
    // Nothing is scheduled in off mode, but give a scheduled check time to run
    // if one ever were, so this fails loudly rather than by timing.
    await page.waitForTimeout(300);
    assert.equal(await page.locator("#native").inputValue(), "真的嗎???");
    assert.equal(await page.locator(".zhtw-mcp-symbol-warning").count(), 0);
  });
});

// contenteditable="false" is the page taking a region out of its own editor.
// Treating it as a field would drop extension UI into the saved document.
test("a contenteditable=false island is not the user's text", { timeout: 60_000 }, async () => {
  await withPage("warn", async (page) => {
    // The island holds "不可編輯...", and focus lands on the editing host that
    // contains it.  The run belongs to the page, so it earns no warning.
    await page.locator("#para").click();
    assert.equal(await page.locator(".zhtw-mcp-symbol-warning").count(), 0);

    // What the user types in the host itself still does.
    await page.keyboard.press("End");
    await page.keyboard.type("???");
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "attached" });

    // And nothing was put inside the document the user is editing.
    assert.equal(await page.locator("#host .zhtw-mcp-symbol-warning").count(), 0);
  });
});

// Focus seeds the text node the caret is in, but moving the caret to another
// node inside the same host raises no focus event, so the first edit there
// arrives with nothing remembered.  The one keystroke that completes a run
// already present in that node is the case a zero-width range would miss, and
// it is the only chance to catch that run: the next keystroke has seeded the
// node and the run is no longer part of what changed.
test("remove mode works in a text node no focus event seeded", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    await page.locator("#para").click();
    await page.locator("#second").click();
    await page.keyboard.press("End");
    await page.keyboard.type(".");

    assert.equal(await page.locator("#second").textContent(), "另一段");
    assert.equal(await page.locator("#para").textContent(), "先寫點字");
  });
});

// The caret alone cannot say what a paste or a deletion changed, and guessing
// there is not a near miss: it lets a keystroke delete a run it never produced.
test("remove mode leaves a run alone when it cannot locate the edit", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    // Set without an input event, so the node arrives holding a run that no
    // edit of the user's produced.
    await page.evaluate(() => {
      document.querySelector("#third").textContent = "尾段...x";
    });
    await page.locator("#para").click();
    await page.locator("#third").click();
    await page.keyboard.press("End");
    await page.keyboard.press("Backspace");

    // Only the character the user deleted, never the run left beside it.
    assert.equal(await page.locator("#third").textContent(), "尾段...");
  });
});

// Extending a run that was already in the field is not producing it: the
// characters behind the caret were never typed here, and remove mode takes only
// what the current edit made.
test("remove mode keeps a run the edit only extended", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await page.evaluate(() => {
      const target = document.querySelector("#native");
      target.value = "...";
      target.focus();
      target.setSelectionRange(0, 0);
    });
    await page.keyboard.type(".");
    assert.equal(await field.inputValue(), "....");
  });
});

// Markup opens a line, and the heading's text follows it there.  The marker
// has to be completed against that text to reach the case: typed left to right
// the run stands alone at the moment it is produced, and is exempt whatever the
// rule says about what may follow it.
test("remove mode leaves a heading marker alone", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    const field = page.locator("#native");
    await field.click();
    await page.keyboard.type("標題");
    await page.keyboard.press("Home");
    await page.keyboard.type("###");
    assert.equal(await field.inputValue(), "###標題");

    // A fence carrying a language tag is the same shape.
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type("rust");
    await page.keyboard.press("Home");
    await page.keyboard.type("```");
    assert.equal(await field.inputValue(), "###標題\n```rust");
  });
});

// url and tel are text the user types like any other.  email and number are
// not here on purpose: setSelectionRange throws on them, which removal needs.
test("remove mode reaches the other text-like inputs", { timeout: 60_000 }, async () => {
  await withPage("remove", async (page) => {
    for (const id of ["tel", "url"]) {
      await page.locator(`#${id}`).click();
      await page.keyboard.type("12...");
      assert.equal(await page.locator(`#${id}`).inputValue(), "12", id);
    }
  });
});

// The overlay exists so that nothing of the extension's ends up in what the
// user saves, which a contenteditable body would undo.
test("the overlay stays out of a contenteditable body", { timeout: 60_000 }, async () => {
  await withPage("warn", async (page) => {
    await page.evaluate(() => document.body.setAttribute("contenteditable", "true"));
    await page.locator("#native").click();
    await page.keyboard.type("真的嗎???");
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "attached" });

    const placement = await page.evaluate(() => {
      const node = document.querySelector(".zhtw-mcp-symbol-warning");
      return { inBody: document.body.contains(node), onRoot: node.parentElement === document.documentElement };
    });
    assert.deepEqual(placement, { inBody: false, onRoot: true });
  });
});

// Off has to mean no listener left behind: one scan should not buy the
// extension a view of everything the user types for the life of the page.
// Behavioural silence cannot tell an unbound listener from a bound one that
// returns early, so this counts the listeners the page actually carries.
test("switching to off tears the observers down", { timeout: 60_000 }, async () => {
  await withPage("warn", async (page) => {
    const cdp = await page.context().newCDPSession(page);
    const documentListeners = async () => {
      const { result } = await cdp.send("Runtime.evaluate", { expression: "document" });
      const { listeners } = await cdp.send("DOMDebugger.getEventListeners", {
        objectId: result.objectId,
      });
      return listeners.map((listener) => listener.type).sort();
    };

    await page.locator("#native").click();
    await page.keyboard.type("真的嗎???");
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "attached" });
    assert.deepEqual(await documentListeners(), ["focusin", "focusout", "input", "scroll"]);

    await page.evaluate(() => {
      window.__zhtwListener({ type: "COLLECT_TEXT", symbol_handling: "off" }, null, () => {});
    });
    await page.locator(".zhtw-mcp-symbol-warning").waitFor({ state: "detached" });

    assert.deepEqual(await documentListeners(), []);

    await page.locator("#native").click();
    await page.keyboard.type("!!!");
    await page.waitForTimeout(300);
    assert.equal(await page.locator("#native").inputValue(), "真的嗎???!!!");
    assert.equal(await page.locator(".zhtw-mcp-symbol-warning").count(), 0);
  });
});
