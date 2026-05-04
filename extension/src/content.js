(() => {
  if (window.__zhtwMcpContentLoaded) {
    return;
  }
  window.__zhtwMcpContentLoaded = true;

  const {
    byteOffsetToCodeUnit,
    normalizeIssue,
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
      });
      text += value;
      byteCursor += byteLength;
      previousNode = node;
    }

    return { text, spans };
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
    if (!issue.length) {
      return [];
    }
    const endByte = issue.offset + issue.length;
    const startIndex = lastTextMap.findIndex(
      (item) => issue.offset >= item.byteStart && issue.offset < item.byteEnd,
    );
    const endIndex = lastTextMap.findIndex(
      (item) => endByte > item.byteStart && endByte <= item.byteEnd,
    );
    if (startIndex < 0 || endIndex < startIndex) {
      return [];
    }

    const ranges = [];
    for (let index = startIndex; index <= endIndex; index += 1) {
      const span = lastTextMap[index];
      if (!span.node.isConnected) {
        return [];
      }

      const segmentStartByte = index === startIndex ? issue.offset : span.byteStart;
      const segmentEndByte = index === endIndex ? endByte : span.byteEnd;
      if (segmentStartByte >= segmentEndByte) {
        continue;
      }

      const text = span.node.nodeValue || "";
      const start = byteOffsetToCodeUnit(text, segmentStartByte - span.byteStart);
      const end = byteOffsetToCodeUnit(text, segmentEndByte - span.byteStart);
      if (start >= end) {
        continue;
      }

      const range = document.createRange();
      range.setStart(span.node, start);
      range.setEnd(span.node, end);
      ranges.push(range);
    }
    return ranges;
  }

  function tooltipForIssue(issue) {
    const suggestion = issue.suggestions.length
      ? `建議：${issue.suggestions.join("、")}`
      : "無自動建議";
    const context = issue.context ? `\n說明：${issue.context}` : "";
    const english = issue.english ? `\nEnglish：${issue.english}` : "";
    return `${issue.found} — ${issue.rule_type} / ${issue.severity}\n${suggestion}${context}${english}`;
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
