// Symbol handling in the page's editable fields: the warning overlay, the
// removal of a run the current edit produced, and the listeners that drive
// both.  content.js owns reading the page and highlighting what the scanner
// reports; the two halves share no state, and this one is reached through
// setSymbolHandling and refreshFocusedWarning.
//
// Injected as a classic script like shared.js and symbols.js, so it publishes
// onto the global rather than exporting.  Re-injection keeps the instance that
// already holds the listeners rather than orphaning it behind a new one.
(function initEditable(root, factory) {
  root.ZhtwExtensionEditable = root.ZhtwExtensionEditable || factory(root);
})(typeof globalThis !== "undefined" ? globalThis : self, function buildEditable(root) {
  // Read off the root the loader passed rather than off `window`: this file is
  // evaluated at import time in the node tests, where the DOM globals are
  // installed afterwards.
  const { changedRange, removableSymbolRuns, runsTheEditProduced } =
    root.ZhtwExtensionSymbols;

  const NATIVE_TEXT_FIELD_SELECTOR =
    "textarea, input:not([type]), input[type=text], input[type=search]";
  // contenteditable="false" is deliberately not here.  It marks a region the
  // page has taken out of its own editor, and treating it as a field would put
  // extension UI inside the document the user is about to save.
  const CONTENTEDITABLE_SELECTOR =
    '[contenteditable=""], [contenteditable="true"], [contenteditable="plaintext-only"]';
  const EDITABLE_FIELD_SELECTOR = `${NATIVE_TEXT_FIELD_SELECTOR}, ${CONTENTEDITABLE_SELECTOR}`;
  const SYMBOL_HANDLING_MODES = ["off", "remove", "warn"];

  let symbolHandling = "warn";
  // What the field held when it was last seen, so that an edit can be located
  // without re-reading the whole field as if the user had just typed all of it.
  const lastFieldValues = new WeakMap();
  let warningNode = null;
  let warningField = null;
  let warningTimer = 0;
  let positionFrame = 0;
  let removing = false;
  const WARNING_IDLE_MS = 120;

  // Off means off: no listener stays behind watching what the user types.  One
  // scan would otherwise buy keystroke observation for the life of the page,
  // which is more than the gesture asked for.
  function setSymbolHandling(mode) {
    symbolHandling = SYMBOL_HANDLING_MODES.includes(mode) ? mode : "warn";
    hideWarning();
    const bound = symbolHandling !== "off";
    bind(document, "input", onEditableInput, bound);
    bind(document, "focusin", onEditableFocus, bound);
    bind(document, "focusout", hideWarning, bound);
    bind(document, "scroll", schedulePositionWarning, bound);
    // Resize is the one window event here, and the node test harness has no
    // window EventTarget, so this target is optional where document is not.
    bind(window, "resize", schedulePositionWarning, bound);
  }

  // Capture on the way down, so a page that stops the event on its own field
  // does not also stop this.
  function bind(target, type, handler, bound) {
    const method = bound ? "addEventListener" : "removeEventListener";
    target[method]?.(type, handler, true);
  }

  const HISTORY_INPUT_TYPES = new Set(["historyUndo", "historyRedo"]);
  // An input that put one character where the caret is, which is the only kind
  // the caret alone is enough to locate.
  const TYPED_INSERTIONS = new Set(["insertText", "insertCompositionText"]);

  function onEditableInput(event) {
    // Removal edits the field, which raises another input event.  Ignoring the
    // reentry keeps one keystroke to one pass.
    if (removing) {
      return;
    }
    const field = editableField(event.target);
    if (!field) {
      return;
    }
    // Undo brings the run back, and removing it again would make the removal
    // impossible to take back.  Redo is the same in reverse.
    if (symbolHandling === "remove" && !HISTORY_INPUT_TYPES.has(event.inputType)) {
      removeSymbolsNearEdit(field, event.inputType);
    }
    rememberFieldValue(field);
    // Only for the field the user is in.  A page is free to raise input on a
    // field nobody is typing in, and an overlay anchored under that one is
    // pointing at nothing the user is doing.
    if (field.contains(document.activeElement)) {
      // Deferred rather than run per keystroke: for a contenteditable the
      // check reads every text node under the editing host, which on a long
      // document costs several milliseconds a key.  A pause is soon enough.
      scheduleWarningUpdate(field);
    }
  }

  function onEditableFocus(event) {
    refreshWarningFor(event.target);
  }

  function refreshFocusedWarning() {
    refreshWarningFor(document.activeElement);
  }

  function refreshWarningFor(target) {
    const field = editableField(target);
    if (!field) {
      hideWarning();
      return;
    }
    // Seeding here is what lets the first keystroke be located.  Without it an
    // edit anywhere in a prefilled field would look like the whole field being
    // rewritten.
    rememberFieldValue(field);
    updateWarning(field);
  }

  function editableField(target) {
    const field = target?.closest?.(EDITABLE_FIELD_SELECTOR);
    if (!field) {
      return null;
    }
    if (isNativeTextField(field)) {
      return field.disabled || field.readOnly ? null : field;
    }
    return field.isContentEditable ? field : null;
  }

  function isNativeTextField(field) {
    return field.matches(NATIVE_TEXT_FIELD_SELECTOR);
  }

  // The two kinds of field differ only in where the text lives and how a range
  // is deleted.  Reading both through one shape keeps what actually matters,
  // the scoping to the edited range and the back-to-front ordering, in a single
  // place rather than in two that can drift.
  //
  // For a contenteditable the surface is the text node the caret sits in.  That
  // is where the edit just happened, so it is the only node a keystroke can
  // have completed a run in, and the only one a run offset maps onto without
  // walking the subtree.
  function editSurface(field) {
    if (isNativeTextField(field)) {
      const text = field.value;
      return {
        key: field,
        text,
        caret: Number.isInteger(field.selectionStart) ? field.selectionStart : text.length,
        remove: (start, end) => deleteFieldRange(field, start, end),
      };
    }
    const selection = window.getSelection?.();
    const node = caretTextNode(field, selection);
    return node
      ? {
          key: node,
          text: node.data,
          caret: selection.anchorOffset,
          remove: (start, end) => deleteTextNodeRange(selection, node, start, end),
        }
      : null;
  }

  function removeSymbolsNearEdit(field, inputType) {
    const surface = editSurface(field);
    if (!surface) {
      return;
    }
    const range = editedRange(
      lastFieldValues.get(surface.key),
      surface.text,
      surface.caret,
      inputType,
    );
    if (!range) {
      return;
    }
    const runs = runsTheEditProduced(removableSymbolRuns(surface.text), range);
    removing = true;
    try {
      // Back to front, so the offsets of an earlier run survive the deletion of
      // a later one.
      for (const run of [...runs].reverse()) {
        surface.remove(run.start, run.end);
      }
    } finally {
      removing = false;
    }
  }

  // Where the edit landed.  With something remembered the two values say it
  // exactly.  Without, the caret is all there is, and it only locates an edit
  // that typed one character into it: the range is that character.  A paste or
  // a deletion can have changed anything, and a guess there is not a near miss
  // but a licence to delete a run the edit never produced, so those give no
  // range and removal does not run.  A field reaches this only for the first
  // edit in a text node no focus event seeded.
  function editedRange(previous, value, caret, inputType) {
    if (previous !== undefined) {
      return changedRange(previous, value);
    }
    return TYPED_INSERTIONS.has(inputType)
      ? { start: Math.max(0, caret - 1), end: caret }
      : null;
  }

  // execCommand is the first choice because it keeps the browser's own undo
  // stack and raises a real InputEvent carrying an inputType, which is what
  // page code listening on the field expects.  Assigning to value does
  // neither.
  //
  // The fallback goes through the prototype setter rather than assigning.  A
  // content script runs in an isolated world, where a value setter the page
  // patched onto the element is not visible at all, so the two are the same
  // thing here; they differ only if this file is ever loaded into the page's
  // own world, where the prototype setter is the one that leaves a framework
  // value tracker stale and therefore lets its change event fire.
  function deleteFieldRange(field, start, end) {
    field.setSelectionRange(start, end);
    if (document.execCommand("delete")) {
      return;
    }
    const next = field.value.slice(0, start) + field.value.slice(end);
    const setter = Object.getOwnPropertyDescriptor(
      Object.getPrototypeOf(field),
      "value",
    )?.set;
    if (setter) {
      setter.call(field, next);
    } else {
      field.value = next;
    }
    field.setSelectionRange(start, start);
    field.dispatchEvent(
      new InputEvent("input", { bubbles: true, inputType: "deleteContentBackward" }),
    );
  }

  function deleteTextNodeRange(selection, node, start, end) {
    const range = document.createRange();
    range.setStart(node, start);
    range.setEnd(node, end);
    selection.removeAllRanges();
    selection.addRange(range);
    document.execCommand("delete");
  }

  function caretTextNode(field, selection) {
    const node = selection?.anchorNode;
    return node?.nodeType === Node.TEXT_NODE && field.contains(node) ? node : null;
  }

  function rememberFieldValue(field) {
    const surface = editSurface(field);
    if (surface) {
      lastFieldValues.set(surface.key, surface.text);
    }
  }

  function scheduleWarningUpdate(field) {
    clearTimeout(warningTimer);
    warningTimer = setTimeout(() => updateWarning(field), WARNING_IDLE_MS);
  }

  function updateWarning(field) {
    clearTimeout(warningTimer);
    if (symbolHandling !== "warn" || !field.isConnected || !hasRemovableSymbols(field)) {
      hideWarning();
      return;
    }
    showWarning(field);
  }

  // Per text node for a contenteditable rather than over its textContent: two
  // paragraphs ending and beginning with a dot read as an ellipsis once the
  // element boundary between them is dropped.
  function hasRemovableSymbols(field) {
    if (isNativeTextField(field)) {
      return removableSymbolRuns(field.value).length > 0;
    }
    const walker = document.createTreeWalker(field, NodeFilter.SHOW_TEXT, {
      // A contenteditable="false" island is the page's own content sitting
      // inside the editor, not something the user typed or can correct.
      acceptNode: (node) =>
        node.parentElement?.isContentEditable
          ? NodeFilter.FILTER_ACCEPT
          : NodeFilter.FILTER_REJECT,
    });
    while (walker.nextNode()) {
      if (removableSymbolRuns(walker.currentNode.data).length) {
        return true;
      }
    }
    return false;
  }

  // The warning is one fixed-position overlay on document.body, not a sibling
  // of the field.  A sibling becomes an extra item in a flex or grid container,
  // is invalid content inside a label, can be torn out from under the page's
  // own framework, and inside a contenteditable would be serialized into
  // whatever the user saves.  data-zhtw-mcp-ui keeps it out of the scan.
  function showWarning(field) {
    if (!warningNode) {
      warningNode = document.createElement("div");
      warningNode.className = "zhtw-mcp-symbol-warning";
      warningNode.setAttribute("role", "status");
      warningNode.dataset.zhtwMcpUi = "true";
      warningNode.textContent = "偵測到連續符號，請確認是否為必要標記。";
    }
    if (warningNode.parentNode !== document.body) {
      document.body.append(warningNode);
    }
    warningField = field;
    positionWarning();
  }

  // Scroll fires far faster than the frame rate, and each pass reads a layout
  // box.  One reposition a frame is all the eye can use.
  function schedulePositionWarning() {
    if (positionFrame) {
      return;
    }
    positionFrame = requestAnimationFrame(() => {
      positionFrame = 0;
      positionWarning();
    });
  }

  function positionWarning() {
    if (!warningNode?.isConnected || !warningField?.isConnected) {
      return;
    }
    const box = warningField.getBoundingClientRect();
    warningNode.style.top = `${box.bottom + 4}px`;
    warningNode.style.left = `${box.left}px`;
  }

  function hideWarning() {
    clearTimeout(warningTimer);
    if (positionFrame) {
      cancelAnimationFrame(positionFrame);
      positionFrame = 0;
    }
    warningNode?.remove();
    warningField = null;
  }

  return { refreshFocusedWarning, setSymbolHandling };
});
