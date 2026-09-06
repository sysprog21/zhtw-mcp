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
    const walker = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_TEXT,
      { acceptNode },
    );

    let previousNode = null;
    while (walker.nextNode()) {
      const node = walker.currentNode;
      const value = node.nodeValue || "";
      const separator = separatorBetween(previousNode, node);
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

  function acceptNode(node) {
    const value = node.nodeValue || "";
    if (!value.trim()) {
      return NodeFilter.FILTER_REJECT;
    }
    const element = node.parentElement;
    if (!element || shouldSkipElement(element) || !isVisible(element)) {
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
      if (current.hidden || current.getAttribute("aria-hidden") === "true") {
        return false;
      }
      const style = getComputedStyle(current);
      if (
        style.display === "none" ||
        style.visibility === "hidden" ||
        style.visibility === "collapse" ||
        Number(style.opacity) === 0
      ) {
        return false;
      }
    }
    return true;
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

  function separatorBetween(previousNode, node) {
    if (!previousNode) {
      return "";
    }
    return nearestBlock(previousNode.parentElement) === nearestBlock(node.parentElement)
      ? ""
      : "\n";
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
