(() => {
  if (window.__zhtwMcpContentLoaded) {
    return;
  }
  window.__zhtwMcpContentLoaded = true;

  const {
    issueSegments,
    langSpans,
    normalizeIssue,
    tooltipForIssue,
    utf8ByteLength,
  } = window.ZhtwExtensionShared;

  const BLOCK_TAGS = new Set([
    "ADDRESS",
    "ARTICLE",
    "ASIDE",
    "BLOCKQUOTE",
    "DD",
    "DETAILS",
    "DIALOG",
    "DIV",
    "DL",
    "DT",
    "FIELDSET",
    "FIGCAPTION",
    "FIGURE",
    "FOOTER",
    "FORM",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "HEADER",
    "HR",
    "LI",
    "MAIN",
    "NAV",
    "OL",
    "P",
    "PRE",
    "SECTION",
    "TABLE",
    "TBODY",
    "TD",
    "TFOOT",
    "TH",
    "THEAD",
    "TR",
    "UL",
  ]);

  let lastTextMap = [];

  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    try {
      if (message?.type === "COLLECT_TEXT") {
        clearHighlights();
        const collected = collectVisibleText();
        lastTextMap = collected.spans;
        sendResponse({
          ok: true,
          text: collected.text,
          node_count: collected.spans.length,
          lang_spans: langSpans(collected.spans),
        });
        return true;
      }

      if (message?.type === "HIGHLIGHT_ISSUES") {
        const issues = (message.issues || []).map(normalizeIssue);
        const summary = highlightIssues(issues);
        sendResponse({ ok: true, ...summary });
        return true;
      }
    } catch (error) {
      sendResponse({ ok: false, error: error.message });
      return true;
    }

    return false;
  });

  function collectVisibleText() {
    const spans = [];
    let text = "";
    let byteCursor = 0;
    // Elements are walked as well as text, for br alone: it is a line break the
    // reader sees and the flattened text would otherwise lose, which would put
    // a marker on a line of its own into the middle of the sentence around it.
    const walker = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT,
      { acceptNode },
    );

    let previousNode = null;
    let previousBlock = null;
    // A br closing the block the previous text sat in is the same break that
    // block's boundary already reports, so the two do not add up.  One that
    // sits in a later block is a line of its own before that block starts, and
    // does.  Sorting them here is what keeps a trailing br from inventing a
    // blank line and a leading one from losing a real one.
    let trailingBreaks = 0;
    let leadingBreaks = 0;
    while (walker.nextNode()) {
      const node = walker.currentNode;
      if (node.nodeType === Node.ELEMENT_NODE) {
        if (previousBlock !== null && previousBlock === nearestBlock(node.parentElement)) {
          trailingBreaks += 1;
        } else {
          leadingBreaks += 1;
        }
        continue;
      }
      const value = node.nodeValue || "";
      const currentBlock = nearestBlock(node.parentElement);
      const blockBreaks = previousNode && previousBlock !== currentBlock ? 1 : 0;
      // Nothing to separate from before the first run, whatever came earlier.
      const separator = previousNode
        ? "\n".repeat(Math.max(trailingBreaks, blockBreaks) + leadingBreaks)
        : "";
      trailingBreaks = 0;
      leadingBreaks = 0;
      if (separator) {
        text += separator;
        byteCursor += utf8ByteLength(separator);
      }

      const byteLength = utf8ByteLength(value);
      spans.push({
        node,
        byteStart: byteCursor,
        byteEnd: byteCursor + byteLength,
        // The declared language of this run, from the nearest ancestor that
        // declared one.  It has to be read here: once the nodes are flattened
        // into one string nothing downstream can see the DOM they came from.
        lang: declaredLang(node),
        // Read through to the live node rather than snapshotting the value.
        // surroundContents splits a text node when a highlight lands in it, so
        // a copy taken here goes stale partway through a highlight pass.  The
        // getter also makes this array directly usable as `issueSegments`
        // input, with no per-issue copy.
        get text() {
          return this.node.nodeValue || "";
        },
      });
      text += value;
      byteCursor += byteLength;
      previousNode = node;
      previousBlock = currentBlock;
    }

    return { text, spans };
  }

  // The lang the nearest ancestor declared, or null when none did.  An
  // explicit lang="" is returned as the empty string rather than null: HTML
  // reads it as "language unknown", which cancels an outer declaration, and
  // closest() has already found the innermost one either way.
  function declaredLang(node) {
    const scope = node.parentElement?.closest("[lang]");
    return scope ? scope.getAttribute("lang") : null;
  }

  // Every element the gate has answered for during this scan.  Answering it
  // means walking that element's ancestors for a skipped tag and a computed
  // style each, and prose returns to the same paragraph after every span it
  // contains, so the repeats are the common case rather than the exception.
  // The map is replaced per scan, since the answer describes the current DOM.
  let elementGate = new WeakMap();

  function elementAllowed(element) {
    if (!element) {
      return false;
    }
    let allowed = elementGate.get(element);
    if (allowed === undefined) {
      allowed = !shouldSkipElement(element) && isVisible(element);
      elementGate.set(element, allowed);
    }
    return allowed;
  }

  function acceptNode(node) {
    if (node.nodeType === Node.ELEMENT_NODE) {
      // Skip rather than reject, so the walk still descends into everything
      // else.  Skipping descends, so a br inside markup the text side excludes
      // does arrive here, and would otherwise contribute a line break from
      // content that reports nothing.
      if (node.tagName !== "BR") {
        return NodeFilter.FILTER_SKIP;
      }
      // A br can only be skipped where its parent is not through one of its own
      // attributes, and almost none carry them, so the general test is kept for
      // those and the rest take the parent's cached answer.  Visibility splits
      // the same way: everything above the br is the parent's, leaving the br's
      // own styles.
      const carriesOwn = node.getAttribute("contenteditable") !== null;
      const allowed = carriesOwn
        ? !shouldSkipElement(node) && isVisible(node)
        : elementAllowed(node.parentElement) && !hiddenInPlace(node);
      return allowed ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
    }
    const value = node.nodeValue || "";
    if (!value.trim()) {
      return NodeFilter.FILTER_REJECT;
    }
    if (!elementAllowed(node.parentElement)) {
      return NodeFilter.FILTER_REJECT;
    }
    return NodeFilter.FILTER_ACCEPT;
  }

  function shouldSkipElement(element) {
    if (
      element.closest(
        "script,style,noscript,textarea,input,select,option,button,code,pre,kbd,samp,var",
      )
    ) {
      return true;
    }
    const editable = element.closest("[contenteditable]");
    return Boolean(editable && editable.getAttribute("contenteditable") !== "false");
  }

  function isVisible(element) {
    for (let current = element; current && current !== document.body; current = current.parentElement) {
      if (hiddenInPlace(current)) {
        return false;
      }
    }
    return true;
  }

  function hiddenInPlace(element) {
    if (element.hidden || element.getAttribute("aria-hidden") === "true") {
      return true;
    }
    const style = getComputedStyle(element);
    return (
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.visibility === "collapse" ||
      Number(style.opacity) === 0
    );
  }

  function highlightIssues(issues) {
    clearHighlights();
    let highlighted = 0;
    let skipped = 0;

    const ordered = [...issues].sort((a, b) => b.offset - a.offset);
    for (const issue of ordered) {
      const ranges = issueToRanges(issue);
      if (!ranges.length) {
        skipped += 1;
        continue;
      }

      let markedSegments = 0;
      for (const range of ranges.reverse()) {
        const mark = document.createElement("mark");
        mark.className = `zhtw-mcp-highlight zhtw-mcp-highlight--${issue.severity}`;
        mark.dataset.zhtwMcpIssue = "true";
        mark.title = tooltipForIssue(issue);

        try {
          range.surroundContents(mark);
          markedSegments += 1;
        } catch {
          // Keep scanning other segments; overlapping DOM mutations can invalidate a range.
        }
      }

      if (markedSegments) {
        highlighted += 1;
      } else {
        skipped += 1;
      }
    }

    return { highlighted_count: highlighted, skipped_count: skipped };
  }

  function issueToRanges(issue) {
    const ranges = [];
    for (const segment of issueSegments(lastTextMap, issue)) {
      const node = lastTextMap[segment.index].node;
      // A node that left the document since COLLECT_TEXT invalidates the whole
      // issue, not just this segment: a partial highlight is worse than none.
      if (!node.isConnected) {
        return [];
      }
      const range = document.createRange();
      range.setStart(node, segment.start);
      range.setEnd(node, segment.end);
      ranges.push(range);
    }
    return ranges;
  }

  function clearHighlights() {
    const marks = [...document.querySelectorAll("mark[data-zhtw-mcp-issue]")];
    for (const mark of marks) {
      const parent = mark.parentNode;
      if (!parent) {
        continue;
      }
      while (mark.firstChild) {
        parent.insertBefore(mark.firstChild, mark);
      }
      parent.removeChild(mark);
      parent.normalize();
    }
  }

  function nearestBlock(element) {
    for (let current = element; current && current !== document.body; current = current.parentElement) {
      if (BLOCK_TAGS.has(current.tagName)) {
        return current;
      }
    }
    return document.body;
  }
})();
