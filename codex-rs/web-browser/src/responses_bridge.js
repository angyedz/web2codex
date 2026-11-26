(() => {
  if (window.top !== window) return;
  let requestId = window.__web2codexRequestId;
  const connectorName = window.__web2codexConnectorName || "Web2Codex";
  const NO_RESPONSE_IDLE_TIMEOUT_MS = 10_000;
  const GENERATION_IDLE_TIMEOUT_MS = 5 * 60_000;
  const POST_TOOL_IDLE_TIMEOUT_MS = 15 * 60_000;
  const sendNative = message => window.ipc.postMessage(JSON.stringify({ ...message, request_id: requestId }));
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = element => {
    if (!element?.isConnected) return false;
    for (let parent = element; parent; parent = parent.parentElement) {
      if (parent.matches('[hidden], [inert], [aria-hidden="true"]')) return false;
      const style = getComputedStyle(parent);
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") return false;
    }
    return true;
  };
  const label = element => (element.getAttribute("aria-label") || element.innerText || element.textContent || "").replace(/\s+/g, " ").trim();
  const elementLabels = element => [
    element?.getAttribute?.("aria-label"),
    element?.getAttribute?.("title"),
    element?.innerText,
    element?.textContent,
  ].filter(value => typeof value === "string").map(value => value.replace(/\s+/g, " ").trim()).filter(Boolean);
  const normalize = text => text.toLowerCase().replace(/\s+/g, " ").trim();
  const hasExactLabel = (element, expected) => elementLabels(element).some(value => normalize(value) === normalize(expected));
  const findComposer = () => {
    const matches = [...document.querySelectorAll('#prompt-textarea, [data-testid="prompt-textarea"], [contenteditable="true"][data-lexical-editor="true"], form[data-chatgpt-composer] [data-composer-markdown][contenteditable="true"][role="textbox"]')].filter(visible);
    return matches.length === 1 ? matches[0] : null;
  };
  const isTemporary = () => location.origin === "https://chatgpt.com" && location.pathname === "/" && new URL(location.href).searchParams.get("temporary-chat") === "true";
  const isLoginSurface = () => {
    const labels = [...document.querySelectorAll('a, button, [role="button"]')]
      .filter(visible).map(element => label(element).toLowerCase());
    return labels.some(value => ["log in", "sign in", "войти", "вход"].includes(value)) ||
      labels.some(value => ["sign up", "create account", "зарегистрироваться"].includes(value));
  };
  const notifyLoginRequired = () => {
    if (!window.__web2codexAuthRequiredNotified) {
      window.__web2codexAuthRequiredNotified = true;
      sendNative({ type: "responses-auth-required" });
    }
    return new Error("ChatGPT sign-in is required. The sign-in window is open; finish signing in, then send the request again.");
  };
  const waitForComposerOrLogin = async (timeout, error, isCurrent = () => true) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (!isCurrent()) throw new Error("Web2Codex page binding superseded");
      if (requestCancelled) throw new Error("Web2Codex request cancelled");
      if (!isTemporary()) {
        if (isLoginSurface()) throw notifyLoginRequired();
        throw new Error("ChatGPT left Temporary Chat; the request was stopped");
      }
      const composer = findComposer();
      if (composer) return composer;
      if (isLoginSurface()) throw notifyLoginRequired();
      await sleep(100);
    }
    throw new Error(error);
  };
  const waitFor = async (read, timeout, error) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      if (requestCancelled) throw new Error("Web2Codex request cancelled");
      if (!isTemporary()) throw new Error("ChatGPT left Temporary Chat; the request was stopped");
      const value = read();
      if (value) return value;
      await sleep(100);
    }
    throw new Error(error);
  };
  const replaceText = (composer, text, append = false) => {
    composer.focus();
    if (composer instanceof HTMLTextAreaElement || composer instanceof HTMLInputElement) {
      const prototype = composer instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, "value").set.call(composer, (append ? composer.value : "") + text);
    } else {
      if (!composer.isContentEditable) throw new Error("Unsupported ChatGPT composer");
      const range = document.createRange();
      range.selectNodeContents(composer);
      if (append) range.collapse(false);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      if (!document.execCommand(text ? "insertText" : "delete", false, text)) throw new Error("ChatGPT composer rejected input");
    }
    composer.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
  };
  const connectorPill = composer => [...composer.querySelectorAll('[data-id^="plugin:"][data-keyword], [app-mention-path^="app://"][app-mention-display-name][contenteditable="false"]')]
    .find(pill => visible(pill) && (pill.getAttribute("data-keyword") || pill.getAttribute("app-mention-display-name")) === connectorName);
  const connectorSelected = composer => Boolean(connectorPill(composer));
  const composerText = (composer, connectorRequired) => {
    let text = composer.value ?? composer.innerText ?? "";
    if (connectorRequired) {
      const pillText = connectorPill(composer)?.innerText;
      if (pillText) {
        const label = pillText.replace(/\r\n/g, "\n").replace(/\u00a0/g, " ").trim().replace(/^@/, "");
        let start = 0;
        while (/\s/.test(text[start] || "") || text.charCodeAt(start) === 160) start++;
        // ChatGPT renders app mentions with a leading @ in some composer
        // builds, while the pill's own innerText omits it. Ignore that marker
        // only when it precedes the selected connector label.
        if (text[start] === "@") start++;
        while (/\s/.test(text[start] || "") || text.charCodeAt(start) === 160) start++;
        if (label && text.slice(start, start + label.length) === label) {
          const end = start + label.length;
          if (end === text.length || /\s/.test(text[end] || "") || text.charCodeAt(end) === 160) {
            text = text.slice(end);
          }
        }
      }
    }
    return text.replace(/\r\n/g, "\n").replace(/\u00a0/g, " ").trim();
  };
  const verifyPrompt = (composer, request) => {
    if (request.connector_required && !connectorSelected(composer)) throw new Error("The connector pill disappeared; the prompt was not sent");
    const actual = composerText(composer, request.connector_required);
    const expected = request.prompt.replace(/\r\n/g, "\n").replace(/\u00a0/g, " ").trim();
    if (actual !== expected) {
      const actualLineBreaks = (actual.match(/\n/g) || []).length;
      const expectedLineBreaks = (expected.match(/\n/g) || []).length;
      // ChatGPT's contenteditable inserts structural line breaks around some
      // pasted blocks. Accept that rendering-only difference when every
      // non-linebreak character remains byte-for-byte in the same order.
      const sameExceptExtraLineBreaks = actualLineBreaks >= expectedLineBreaks &&
        actual.replace(/\n/g, "") === expected.replace(/\n/g, "");
      if (sameExceptExtraLineBreaks) return;
      let mismatch = 0;
      while (mismatch < actual.length && mismatch < expected.length && actual[mismatch] === expected[mismatch]) mismatch++;
      const sameWithoutLineBreaks = actual.replace(/\n/g, "") === expected.replace(/\n/g, "");
      const whitespaceNormalizedMatch = actual.replace(/\s+/g, " ").trim() === expected.replace(/\s+/g, " ").trim();
      const textContent = String(composer.textContent ?? "").trim();
      throw new Error(`ChatGPT did not preserve the complete prompt text; the prompt was not sent (composer ${actual.length}/${expected.length} characters; first mismatch ${mismatch}; linebreaks ${actualLineBreaks}/${expectedLineBreaks}; same without linebreaks ${sameWithoutLineBreaks}; whitespace-normalized ${whitespaceNormalizedMatch}; textContent ${textContent.length} chars)`);
    }
  };

  // The same @ menu and pill contract used by chatgpt-web-coder. A literal
  // @name in text does not count as selecting a connector.
  const attachConnector = async composer => {
    if (connectorSelected(composer) && !composerText(composer, true)) return composer;
    replaceText(composer, "");
    try {
      for (const character of `@${connectorName}`) {
        replaceText(composer, character, true);
        await sleep(40);
      }
      const row = await waitFor(() => {
        const rows = [...document.querySelectorAll('.__menu-item[tabindex="0"], [data-mention-list-scroll-area] button[data-list-navigation-item="true"], [data-list-navigation-item="true"], [role="option"], [role="menuitem"]')]
          .filter(visible).filter(row => hasExactLabel(row, connectorName) || [...row.querySelectorAll("span, div, button, [role]")].some(child => visible(child) && hasExactLabel(child, connectorName)));
        const outer = rows.filter(row => !rows.some(other => other !== row && other.contains(row)));
        return outer.length === 1 ? outer[0] : null;
      }, 7000, `ChatGPT did not show the configured connector ${connectorName} in the @ menu. The request was not sent; check Temporary Chat personalization and the connector selector in this browser profile.`);
      row.click();
      await waitFor(() => {
        allowConnectorOnce();
        const current = findComposer();
        return current && connectorSelected(current) ? current : null;
      }, 7000, `ChatGPT did not attach the ${connectorName} connector; the prompt was not sent`);
      return findComposer();
    } catch (error) {
      const current = findComposer();
      if (current && !connectorSelected(current)) {
        try { replaceText(current, ""); } catch {}
      }
      throw error;
    }
  };

  const ensurePersonalizedTemporaryChat = async () => {
    const unpersonalized = [...document.querySelectorAll('button, [role="button"]')]
      .find(button => visible(button) && elementLabels(button).some(value => /^(Unpersonalized|Без персонализации|Неперсонализированный|非个性化)$/i.test(value)));
    if (!unpersonalized) return;
    unpersonalized.click();
    const personalized = await waitFor(() => [...document.querySelectorAll('button, [role="button"], [role="menuitem"], [role="option"]')]
      .find(option => visible(option) && elementLabels(option).some(value => /^(Personalized|Personalize|С персонализацией|Персонализированный|个性化)$/i.test(value))), 4000, "Choose Personalized for Temporary Chats so the custom MCP connector is available");
    personalized.click();
    await sleep(300);
  };

  const ensureRequestedModel = async requested => {
    if (!window.__web2codexModelPicker) throw new Error("ChatGPT model picker is unavailable");
    await window.__web2codexModelPicker.select(requested);
  };
  const ensureRequestedEffort = async requested => {
    if (!requested) return;
    if (!window.__web2codexModelPicker) throw new Error("ChatGPT reasoning effort picker is unavailable");
    await window.__web2codexModelPicker.setEffort(requested);
  };

  const attachFiles = async (composer, attachments, connectorRequired) => {
    if (!attachments?.length) return composer;
    let form = composer.closest("form") || document;
    const accepts = (input, attachment) => {
      const accept = (input.getAttribute("accept") || "").split(",").map(value => value.trim().toLowerCase()).filter(Boolean);
      if (!accept.length) return true;
      const extension = attachment.name.split(".").pop()?.toLowerCase();
      return accept.some(value => value === attachment.mime.toLowerCase() || value === `.${extension}` ||
        (value.endsWith("/*") && attachment.mime.toLowerCase().startsWith(value.slice(0, -1))));
    };
    const fileInput = root => [...root.querySelectorAll('input[type="file"]')].find(input => attachments.every(attachment => accepts(input, attachment))) || null;
    let input = fileInput(form);
    if (!input) {
      const add = [...form.querySelectorAll("button")].find(button => visible(button) && (
        /attach|upload/i.test(button.getAttribute("data-testid") || "") ||
        /attach|upload|add files|прикреп|добавить файлы/i.test(label(button))));
      if (add) add.click();
      input = await waitFor(() => fileInput(document), 5000, "ChatGPT file upload control was not found; the prompt was not sent");
    }
    const transfer = new DataTransfer();
    for (const attachment of attachments) {
      const comma = attachment.data_url.indexOf(",");
      if (comma < 0) throw new Error("Invalid media attachment; the prompt was not sent");
      let decoded;
      try { decoded = atob(attachment.data_url.slice(comma + 1)); }
      catch { throw new Error("Invalid media attachment encoding; the prompt was not sent"); }
      const bytes = Uint8Array.from(decoded, character => character.charCodeAt(0));
      transfer.items.add(new File([bytes], attachment.name, { type: attachment.mime }));
    }
    input.files = transfer.files;
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
    await waitFor(() => {
      const current = findComposer();
      form = current?.closest("form") || document;
      const cards = [...form.querySelectorAll('[role="group"], .composer-attachment-surface, [data-testid*="attachment"], [data-testid*="file"]')];
      return attachments.every(attachment => cards.some(card => {
        if (!visible(card) || card.querySelector('[aria-busy="true"], [data-state="loading"], .animate-spin')) return false;
        const descriptions = [card.getAttribute("aria-label"), card.getAttribute("title"), card.textContent];
        for (const child of card.querySelectorAll("[aria-label], [title]")) {
          descriptions.push(child.getAttribute("aria-label"), child.getAttribute("title"));
        }
        return descriptions.some(description => description?.includes(attachment.name));
      }));
    }, 60000, "ChatGPT did not confirm the file attachments; the prompt was not sent");
    const current = findComposer();
    if (!current || (connectorRequired && !connectorSelected(current))) throw new Error("The connector disappeared while attaching images; the prompt was not sent");
    return current;
  };

  // ChatGPT's connector consent only forwards calls to this local harness.
  // Execution still passes through Codex's own approvals and sandbox.
  const allowConnectorOnce = () => {
    for (const button of document.querySelectorAll("button")) {
      if (!visible(button) || !/^(Allow once|Allow|Разрешить один раз|Разрешить)$/i.test(label(button))) continue;
      const scope = button.closest('[role="dialog"], [data-message-author-role="assistant"]');
      if (scope?.textContent?.includes(connectorName)) { button.click(); return; }
    }
  };
  const stopButton = () => [...document.querySelectorAll("button")].find(button => visible(button) && (
    /stop-button|stop-generating/.test(button.getAttribute("data-testid") || "") ||
    /^(Stop(?: generating| streaming| responding| response)?|Остановить(?: генерацию| ответ)?)/i.test(label(button))));
  const stopVisible = () => Boolean(stopButton());
  const approvalVisible = () => [...document.querySelectorAll('[role="dialog"], [data-message-author-role="assistant"] button')]
    .some(control => visible(control) && /^(Allow once|Allow|Always allow|Разрешить|Всегда разрешать)/i.test(label(control)));
  const outermost = nodes => {
    const candidates = new Set(nodes);
    return nodes.filter(node => {
      for (let parent = node.parentElement; parent; parent = parent.parentElement) {
        if (candidates.has(parent)) return false;
      }
      return true;
    });
  };
  const assistantMessages = () => outermost([...document.querySelectorAll(
    '[data-message-author-role="assistant"], [data-message-author="assistant"], [data-conversation-role="assistant"], '
    + '[data-content-search-unit-key$=":assistant"], [data-chatgpt-search-unit-key$=":assistant"], '
    + '[data-testid^="conversation-turn-"][data-turn="assistant"]:not([data-turn-key] *), '
    + '[data-testid^="conversation-turn-"][data-message-author-role="assistant"]:not([data-turn-key] *), '
    + '[data-testid^="conversation-turn-"]:has([data-message-author-role="assistant"]):not([data-turn-key] *), '
    + '[data-turn-key]:has([data-conversation-role="assistant"], [data-chatgpt-agent-turn-start])'
  )].filter(visible));
  const userMessages = () => outermost([...document.querySelectorAll(
    '[data-message-author="user"], [data-message-author-role="user"], [data-conversation-role="user"], '
    + '[data-content-search-unit-key$=":user"], [data-chatgpt-search-unit-key$=":user"], '
    + '[data-testid^="conversation-turn-"][data-turn="user"]:not([data-turn-key] *), '
    + '[data-testid^="conversation-turn-"][data-message-author-role="user"]:not([data-turn-key] *), '
    + '[data-testid^="conversation-turn-"]:has([data-message-author-role="user"]):not([data-turn-key] *), '
    + '[data-turn-key]:has([data-user-message-bubble])'
  )].filter(visible));
  const messageIdentity = (message, role = "assistant") => message.getAttribute("data-message-id") ||
    (message.getAttribute("data-turn-key") && `group:${role}:${message.getAttribute("data-turn-key")}`) ||
    message.getAttribute("data-content-search-unit-key") || message.getAttribute("data-chatgpt-search-unit-key");
  const messageMatches = (entry, old) => entry.id && old.id ? entry.id === old.id : entry.message === old.message;
  const userSnapshot = () => userMessages().map(message => ({ message, id: messageIdentity(message, "user") }));
  const markdownSelector = '.markdown, [data-markdown-text-style="assistant-message"], .puik-root.not-markdown > [class*="_DilResponseRoot"]';
  const controlsSelector = 'button, [role="button"], [role="toolbar"], [role="status"], [role="dialog"], [role="alert"], [role="progressbar"], [data-message-author-role="tool"]';
  const reasoningSelector = '[data-testid*="reason" i], [data-testid*="think" i], [data-testid*="thought" i], [aria-label*="reason" i], [aria-label*="think" i], [aria-label*="thought" i]';
  const blockTags = new Set(["DIV", "P", "PRE", "UL", "OL", "LI", "TABLE", "TR", "BLOCKQUOTE", "SECTION", "HEADER", "H1", "H2", "H3", "H4", "H5", "H6"]);
  const terminalSafeText = text => text
    .replace(/\x1B\][\s\S]*?(?:\x07|\x1B\\|$)/g, "")
    .replace(/\u009D[\s\S]*?(?:\x07|\u009C|\x1B\\|$)/g, "")
    .replace(/\x1B(?:P|X|\^|_)[\s\S]*?(?:\x1B\\|$)/g, "")
    .replace(/[\u0090\u0098\u009E\u009F][\s\S]*?(?:\u009C|\x1B\\|$)/g, "")
    .replace(/\x1B\[[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\u009B[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\x1B[ -/]*[@-~]/g, "")
    .replace(/[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g, "");
  const renderedList = (list, depth = 0) => {
    const items = [...(list.children || [])].filter(child => child.tagName === "LI");
    const reversed = list.tagName === "OL" && list.getAttribute("reversed") !== null;
    const explicitStart = Number.parseInt(list.getAttribute("start") || "", 10);
    let nextNumber = Number.isFinite(explicitStart) ? explicitStart : reversed ? items.length : 1;
    const numberStep = reversed ? -1 : 1;
    return items.map(item => {
      const nestedLists = [...(item.children || [])].filter(child => child.tagName === "UL" || child.tagName === "OL");
      const content = [...(item.childNodes || [])]
        .filter(child => child.nodeType !== 1 || (child.tagName !== "UL" && child.tagName !== "OL"))
        .map(child => child.nodeType === 3 ? child.nodeValue || "" : renderedText(child))
        .join("")
        .replace(/\s*\n\s*/g, " ")
        .trim();
      const explicitNumber = Number.parseInt(item.getAttribute("value") || "", 10);
      let marker = "-";
      if (list.tagName === "OL") {
        if (Number.isFinite(explicitNumber)) nextNumber = explicitNumber;
        marker = `${nextNumber}.`;
        nextNumber += numberStep;
      }
      const nested = nestedLists.map(child => renderedList(child, depth + 1)).filter(Boolean).join("\n");
      return `${"  ".repeat(depth)}${marker}${content ? ` ${content}` : ""}${nested ? `\n${nested}` : ""}`;
    }).join("\n");
  };
  const renderedText = node => {
    if (node.matches(controlsSelector) || node.matches(reasoningSelector)) return "";
    if (node.tagName === "UL" || node.tagName === "OL") return renderedList(node);
    if (/^H[1-6]$/.test(node.tagName)) {
      const heading = (node.innerText || node.textContent || "").replace(/\s+/g, " ").trim();
      return heading ? `${"#".repeat(Number(node.tagName.slice(1)))} ${heading}` : "";
    }
    if (node.tagName === "BLOCKQUOTE") {
      const quote = [...(node.childNodes || [])]
        .map(child => child.nodeType === 3 ? child.nodeValue || "" : renderedText(child))
        .join("")
        .trim();
      return quote.split(/\r?\n/).map(line => line ? `> ${line}` : ">").join("\n");
    }
    // Code blocks may mount a toolbar while tokens are still arriving.
    // Use the code body itself so the toolbar cannot become part of a delta.
    if (node.tagName === "PRE") {
      const code = node.querySelector("code");
      const source = (code?.innerText || node.innerText || "").replace(/\n+$/, "");
      return source.split(/\r?\n/).map(line => `    ${line}`).join("\n");
    }
    if (node.tagName === "A") {
      const text = (node.innerText || node.textContent || "").replace(/\s+/g, " ").trim();
      const href = node.getAttribute("href");
      if (!href) return text;
      let url;
      try { url = new URL(href, location.href); } catch { return text; }
      if (url.protocol !== "http:" && url.protocol !== "https:") return text;
      return text && text !== url.href ? `${text} (${url.href})` : url.href;
    }
    // ChatGPT can ignore the plain-text prompt constraints and emit an HTML
    // table. Convert it into labeled lines so tabs never become a fake layout
    // in the terminal transcript.
    if (node.tagName === "TABLE") {
      const closestTag = (element, tagName) => {
        for (let current = element; current; current = current.parentElement) {
          if (current.tagName === tagName) return current;
        }
        return null;
      };
      const rows = [...node.querySelectorAll("tr")]
        .filter(row => visible(row) && closestTag(row, "TABLE") === node)
        .map(row => {
          const cells = [...row.querySelectorAll("th, td")]
            .filter(cell => visible(cell) && closestTag(cell, "TR") === row && closestTag(cell, "TABLE") === node);
          return {
            isHeader: cells.length > 0 && cells.every(cell => cell.tagName === "TH"),
            hasSpans: cells.some(cell => Number.parseInt(cell.getAttribute("colspan") || "1", 10) > 1 ||
              Number.parseInt(cell.getAttribute("rowspan") || "1", 10) > 1),
            cells: cells.map(cell => renderedText(cell).replace(/\s+/g, " ").trim()),
          };
        });
      const headerIndex = rows.findIndex(row => row.cells.some(Boolean));
      const useColumnHeaders = headerIndex >= 0 && rows[headerIndex].isHeader && !rows.some(row => row.hasSpans);
      const headers = useColumnHeaders ? rows.splice(headerIndex, 1)[0].cells : [];
      if (rows.length === 0) return headers.join(" · ");
      return rows.map(({ cells }) => {
        const values = cells.map((value, index) => {
          if (!value) return "";
          const header = headers[index] || (headers.length ? `Column ${index + 1}` : "");
          return header ? `${header}: ${value}` : value;
        }).filter(Boolean);
        return values.length ? `- ${values.join("; ")}` : "";
      }).filter(Boolean).join("\n");
    }
    if (node.tagName === "CODE") {
      const source = node.innerText || node.textContent || "";
      const fence = "`".repeat(Math.max(1, ...[...source.matchAll(/`+/g)].map(match => match[0].length + 1)));
      return `${fence}${source}${fence}`;
    }
    // Keep separators identical when a button mounts/unmounts in a sibling.
    // innerText on leaves preserves the browser's visible line breaks.
    if (!node.firstElementChild) {
      // A few ChatGPT assistant wrappers aggregate tool activity in innerText but
      // expose no readable child tree. Do not mistake that aggregate for an answer.
      if (node.childElementCount > 0 && !(node.childNodes || []).length) return "";
      return node.innerText || "";
    }
    let text = "";
    for (const child of node.childNodes) {
      if (child.nodeType === 3) { text += child.nodeValue; continue; }
      if (child.nodeType !== 1 || child.matches('[hidden], [inert], [aria-hidden="true"]')) continue;
      const style = getComputedStyle(child);
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") continue;
      if (child.tagName === "BR") { text += "\n"; continue; }
      const content = renderedText(child);
      if (!content) continue;
      if (blockTags.has(child.tagName) && text && !text.endsWith("\n")) text += "\n";
      if ((child.tagName === "TD" || child.tagName === "TH") && text) text += "\t";
      text += content;
      if (blockTags.has(child.tagName) && !text.endsWith("\n")) text += "\n";
    }
    // Dropped controls at the end of a block can leave their preceding space behind.
    return text.replace(/[ \t]+$/, "");
  };
  const assistantMessageText = message => {
    // New ChatGPT builds group the user and assistant turns under one stable
    // data-turn-key. The conversation-role marker and Markdown are siblings
    // inside an assistant-owned search unit, so read the grouped turn while
    // admitting content only from that unit or an agent activity container.
    const groupedTurn = message.matches('[data-turn-key]');
    const contentRoot = message;
    const activityContainers = groupedTurn
      ? [...message.querySelectorAll('[data-chatgpt-agent-turn-start]')].map(marker => marker.parentElement).filter(Boolean)
      : [];
    const belongsToAssistant = node => {
      if (!groupedTurn) return true;
      const unit = node.closest('[data-content-search-unit-key]');
      if (unit && [...(unit.children || [])].some(child => child.getAttribute('data-conversation-role') === 'assistant')) return true;
      return activityContainers.some(container => container.contains(node));
    };
    const inProse = node => {
      for (let parent = node; parent; parent = parent.parentElement) {
        if (parent.matches(controlsSelector) || parent.matches(reasoningSelector)) return false;
        if (parent === contentRoot) break;
      }
      return visible(node) && belongsToAssistant(node);
    };
    const markdown = outermost([...contentRoot.querySelectorAll(markdownSelector)].filter(inProse));
    if (markdown.length) return markdown.map(renderedText).join("\n\n").trim();
    // Some variants render plain text directly. A container of tool/approval
    // widgets is also marked "assistant", so never use its aggregate innerText.
    if (!groupedTurn && !contentRoot.childElementCount) return inProse(contentRoot) ? (contentRoot.innerText || "").trim() : "";
    const blocks = outermost([...contentRoot.querySelectorAll("p, pre, ul, ol, blockquote, table, h1, h2, h3, h4, h5, h6")].filter(inProse));
    const structuredText = blocks.map(renderedText).join("\n\n").trim();
    if (structuredText) return structuredText;
    // ChatGPT can render a short plain-text answer in generic div/span wrappers.
    // Traverse visible text while still dropping controls and reasoning sections.
    const fallbackRoots = groupedTurn
      ? outermost([...contentRoot.querySelectorAll('[data-content-search-unit-key]')]
        .filter(unit => [...(unit.children || [])].some(child => child.getAttribute('data-conversation-role') === 'assistant') && visible(unit)))
      : [contentRoot];
    const fallback = fallbackRoots.map(renderedText).join("\n\n").trim();
    return isToolActivityPayload(fallback) ? "" : fallback;
  };
  const assistantSnapshot = () => assistantMessages().map(message => ({
    message, id: messageIdentity(message), text: assistantMessageText(message),
  }));
  const responseMessages = baseline => {
    // Message identity survives historical DOM removal, reordering and remounts.
    const messages = assistantSnapshot().map(entry => ({
      ...entry, previous: baseline.find(old => messageMatches(entry, old)),
    }));
    const matched = new Set(messages.map(entry => entry.previous).filter(Boolean));
    // Anonymous replacement nodes have no stable identity. Never claim a
    // historical entry that is already represented by another visible node.
    for (const [index, entry] of messages.entries()) {
      const old = baseline[index];
      if (!entry.previous && !entry.id && old && !old.id && !matched.has(old)) {
        entry.previous = old;
        matched.add(old);
      }
    }
    return messages;
  };
  const responseText = (messages, baseline, submittedUser) => terminalSafeText(messages.flatMap(({ message, text, previous }) => {
    if (!text) return [];
    if (!previous) return [text];
    // A reused or rehydrated message that remains before the submitted user
    // turn is still history, even if ChatGPT changes its visible text.
    const position = submittedUser?.compareDocumentPosition?.(message) || 0;
    if ((position & 2) && !(position & 1)) return [];
    // A reused node positioned after a newly submitted user message belongs to
    // this turn, even if its answer is identical to the preceding answer.
    if (previous === baseline[baseline.length - 1] && previous.message === message &&
        (position & 4) && !(position & 1)) return [text];
    // Only the most recent historical message can be reused for a new turn.
    if (previous !== baseline[baseline.length - 1] || text === previous.text) return [];
    // An identified historical message remounted during a new turn is still
    // history, even if hydration changed its rendered text.
    if (previous.id && message !== previous.message) return [];
    // A replacement may start with the same letters (e.g. "OK" → "OK!").
    // Only a separate appended paragraph is evidence of a retained prefix.
    const suffix = previous.text && text.startsWith(`${previous.text}\n\n`) ? text.slice(previous.text.length).trim() : text;
    return suffix ? [suffix] : [];
  }).join("\n\n"));

  const reasoningHeading = messages => {
    const candidates = [];
    for (const { message, previous } of messages) {
      if (previous) continue;
      const reasoningScopes = [...message.querySelectorAll(reasoningSelector)].filter(visible);
      for (const scope of reasoningScopes) {
        if (scope.matches('h1, h2, h3, h4, h5, h6, [role="heading"]')) candidates.push(scope);
        candidates.push(...scope.querySelectorAll('h1, h2, h3, h4, h5, h6, [role="heading"]'));
      }
      candidates.push(...message.querySelectorAll('button[aria-expanded], [role="button"][aria-expanded], details > summary'));
    }
    const isReasoningLabel = text => /^(?:thought|reasoned|thinking|reasoning)(?:\s+(?:for|about)\b.*)?$|^(?:думал\w*|размышлял\w*|обдумывал\w*|размышлени\w*)(?:\s+.*)?$/iu.test(text);
    for (const candidate of candidates) {
      if (!visible(candidate)) continue;
      const isSemanticHeading = candidate.matches('h1, h2, h3, h4, h5, h6, [role="heading"]');
      const source = isSemanticHeading
        ? (candidate.innerText || candidate.textContent || candidate.getAttribute("aria-label"))
        : (candidate.getAttribute("aria-label") || candidate.innerText || candidate.textContent);
      const text = terminalSafeText(source || "")
        .split(/\r?\n/, 1)[0].replace(/\s+/g, " ").trim();
      if (!text || text.length > 120 || (!isSemanticHeading && !isReasoningLabel(text))) continue;
      return text;
    }
    return "";
  };

  // Append-only clients cannot retract text if Markdown is reparsed. Only
  // publish a prefix seen in two snapshots and hold back a short mutable tail.
  const stableEnd = (previous, current, sent) => {
    if (!current.startsWith(previous) || previous.length <= sent) return sent;
    const lineEnd = previous.lastIndexOf("\n") + 1;
    const safePrefixEnd = previous.length - 8;
    const wordEnd = safePrefixEnd > sent ? previous.lastIndexOf(" ", safePrefixEnd) + 1 : 0;
    return Math.max(sent, lineEnd, wordEnd);
  };
  const isToolActivityPayload = text =>
    /^\s*Tool (?:call|result)\s*\{/i.test(text) ||
    /"(?:wire_name|structuredContent)"\s*:/.test(text);
  const appendTranscriptText = (transcript, next) => {
    let segment = next.trim();
    if (!segment) return transcript;
    if (!transcript) return segment;
    if (transcript === segment) return transcript;
    if (segment.startsWith(transcript)) return segment;
    const existingParagraphs = new Set(transcript.split(/\n{2,}/).map(paragraph => paragraph.trim()).filter(Boolean));
    const paragraphs = segment.split(/\n{2,}/);
    const originalParagraphCount = paragraphs.length;
    while (paragraphs.length > 1 && existingParagraphs.has(paragraphs[0].trim())) paragraphs.shift();
    if (paragraphs.length < originalParagraphCount) {
      segment = paragraphs.join("\n\n").trim();
      if (!segment) return transcript;
    }
    if (existingParagraphs.has(segment)) return transcript;
    return `${transcript}\n\n${segment}`;
  };
  const withPreservedTranscript = (preserved, current) => {
    if (!preserved) return current;
    if (!current || current.startsWith(preserved)) return current || preserved;
    return appendTranscriptText(preserved, current);
  };

  let busy = false;
  let promptSubmitted = false;
  let requestCancelled = false;
  let cancelledResponseNeedsStop = false;
  let activeRequestPrompt = "";
  let bindingRevision = 0;
  let bindingAttempt = 0;
  let lastSubmittedRequestId = null;
  window.__web2codexCancel = cancelledRequestId => {
    if (cancelledRequestId !== requestId || !busy || requestCancelled) return false;
    requestCancelled = true;
    if (promptSubmitted) {
      cancelledResponseNeedsStop = true;
      stopButton()?.click();
    } else {
      const composer = findComposer();
      const tail = activeRequestPrompt.replace(/\s+/g, " ").trim().slice(-120);
      const text = (composer?.innerText || composer?.value || "").replace(/\s+/g, " ");
      if (composer && tail && text.includes(tail)) {
        try { replaceText(composer, ""); } catch {}
      }
    }
    return true;
  };
  window.__web2codexBindRequest = async nextRequestId => {
    if (typeof nextRequestId !== "string" || !nextRequestId) return false;
    const attempt = ++bindingAttempt;
    const previousWasCancelled = requestCancelled || cancelledResponseNeedsStop;
    // Native completion can be acknowledged just before the observer's final
    // sleep ends. Let that observer unwind before rebinding the retained page.
    const deadline = Date.now() + (previousWasCancelled ? 30000 : 5000);
    while (busy && attempt === bindingAttempt && Date.now() < deadline) await sleep(100);
    if (busy || attempt !== bindingAttempt) return false;
    // A same-chat interruption must wait until ChatGPT has stopped generating
    // before the next user message is typed into its composer.
    if (previousWasCancelled) {
      const stopDeadline = Date.now() + 30000;
      while (stopVisible() && attempt === bindingAttempt && Date.now() < stopDeadline) await sleep(100);
      if (stopVisible() || attempt !== bindingAttempt) return false;
    }
    cancelledResponseNeedsStop = false;
    const binding = ++bindingRevision;
    requestId = nextRequestId;
    window.__web2codexRequestId = nextRequestId;
    window.__web2codexToolState = null;
    window.__web2codexAuthRequiredNotified = false;
    requestCancelled = false;
    try {
      await waitForComposerOrLogin(60000, "ChatGPT did not expose a signed-in composer. Use /web login in the terminal to restore the web session", () => binding === bindingRevision);
      if (binding !== bindingRevision) return false;
      sendNative({ type: "responses-bridge-ready", conversation_has_history: assistantMessages().length > 0 });
      return true;
    } catch (error) {
      if (binding !== bindingRevision) return false;
      if (!window.__web2codexAuthRequiredNotified) {
        sendNative({ type: "responses-result", error: terminalSafeText(String(error?.message || error)) });
      }
      return false;
    }
  };
  const submitCompactionStages = async (request, stages) => {
    if (stages.length !== 3) throw new Error("Compaction transport must contain exactly four parts");
    for (const [index, stage] of stages.entries()) {
      if (typeof stage?.prompt !== "string" || typeof stage?.acknowledgement !== "string") {
        throw new Error("Compaction part is malformed; no summary was accepted");
      }
      let composer = await waitForComposerOrLogin(30000, "ChatGPT composer did not appear for a compaction part");
      replaceText(composer, stage.prompt);
      verifyPrompt(composer, { ...request, prompt: stage.prompt, connector_required: false });
      const submit = await waitFor(() => {
        const form = findComposer()?.closest("form");
        return [...(form || document).querySelectorAll('button[data-testid="send-button"], button[data-testid="composer-send-button"], button[type="submit"]')]
          .find(button => visible(button) && !button.disabled);
      }, 5000, "ChatGPT send button stayed unavailable for a compaction part");
      composer = findComposer();
      if (!composer) throw new Error("ChatGPT composer disappeared before a compaction part was sent");
      verifyPrompt(composer, { ...request, prompt: stage.prompt, connector_required: false });
      const baseline = assistantSnapshot();
      const userBaseline = userSnapshot();
      lastSubmittedRequestId = requestId;
      submit.click();
      promptSubmitted = true;
      if (index === 0) sendNative({ type: "responses-prompt-submitted" });

      const deadline = Date.now() + 120000;
      let mismatchText = "";
      let mismatchSince = 0;
      let acknowledged = false;
      while (Date.now() < deadline && !requestCancelled) {
        if (!isTemporary()) throw new Error("ChatGPT left Temporary Chat while receiving compaction context");
        const messages = responseMessages(baseline);
        const submittedUser = userSnapshot()
          .filter(entry => entry.id && !userBaseline.some(old => messageMatches(entry, old)))
          .at(-1)?.message;
        const text = responseText(messages, baseline, submittedUser).trim();
        if (text && !stopVisible()) {
          if (text === stage.acknowledgement) {
            acknowledged = true;
            break;
          }
          if (text && text === mismatchText && Date.now() - mismatchSince >= 1200) {
            throw new Error(`ChatGPT did not acknowledge compaction part ${index + 1}/4; the context handoff stopped without summarizing`);
          }
          if (text !== mismatchText) {
            mismatchText = text;
            mismatchSince = Date.now();
          }
        }
        await sleep(100);
      }
      if (requestCancelled) return false;
      if (!acknowledged) throw new Error(`ChatGPT did not acknowledge compaction part ${index + 1}/4 within two minutes`);
    }
    return true;
  };
  window.__web2codexSend = async request => {
    if (busy || request.request_id !== requestId || request.request_id === lastSubmittedRequestId) return;
    busy = true;
    promptSubmitted = false;
    requestCancelled = false;
    cancelledResponseNeedsStop = false;
    activeRequestPrompt = request.prompt;
    window.__web2codexCompleted = false;
    window.__web2codexToolState = null;
    try {
      let composer = await waitForComposerOrLogin(30000, "ChatGPT composer did not appear; the request was not sent");
      if (requestCancelled) return;
      if (request.connector_required) await ensurePersonalizedTemporaryChat();
      if (requestCancelled) return;
      const compaction = Array.isArray(request.stages) && request.stages.length > 0;
      if (compaction) {
        if (!window.__web2codexModelPicker?.selectInstantForCompaction) {
          throw new Error("ChatGPT cannot verify an available Instant model for compaction");
        }
        await window.__web2codexModelPicker.selectInstantForCompaction(request.model);
      } else {
        await ensureRequestedModel(request.model);
      }
      if (requestCancelled) return;
      if (!compaction) await ensureRequestedEffort(request.reasoning_effort);
      if (requestCancelled) return;
      if (Array.isArray(request.stages) && request.stages.length) {
        const completed = await submitCompactionStages(request, request.stages);
        if (!completed || requestCancelled) return;
      }
      // Model and reasoning menus can re-render the composer. Never type into
      // the element captured before those selections.
      composer = await waitForComposerOrLogin(10000, "ChatGPT composer did not return after selecting the model");
      if (requestCancelled) return;
      if (request.connector_required) composer = await attachConnector(composer);
      if (requestCancelled) return;
      replaceText(composer, request.prompt, Boolean(request.connector_required));
      verifyPrompt(composer, request);
      composer = await attachFiles(composer, request.attachments, request.connector_required);
      if (requestCancelled) return;
      const submit = await waitFor(() => {
        const form = findComposer()?.closest("form");
        return [...(form || document).querySelectorAll('button[data-testid="send-button"], button[data-testid="composer-send-button"], button[type="submit"]')]
          .find(button => visible(button) && !button.disabled);
      }, 5000, "ChatGPT send button stayed unavailable");
      if (requestCancelled) return;
      composer = findComposer();
      if (!composer) throw new Error("ChatGPT composer disappeared; the prompt was not sent");
      verifyPrompt(composer, request);
      const baseline = assistantSnapshot();
      const userBaseline = userSnapshot();
      lastSubmittedRequestId = requestId;
      submit.click();
      promptSubmitted = true;
      sendNative({ type: "responses-prompt-submitted" });

      let text = "";
      let rawText = "";
      let streamedText = "";
      let preservedAssistantText = "";
      let lastObservedMessages = [];
      const preservedAssistantMessageRefs = new Set();
      let lastObservedRawText = "";
      let lastObservedGenerating = false;
      let lastToolState = null;
      let lastChange = Date.now();
      let revision = -1;
      let candidateHeading = "";
      let candidateHeadingSince = 0;
      let sentHeading = "";
      const startDeadline = Date.now() + 120000;
      let responseStarted = false;
      // Tool rounds and approvals can outlive a single HTTP response. Observe
      // this same ChatGPT response until the native completion fence accepts it.
      while (!window.__web2codexCompleted && !requestCancelled) {
        if (!isTemporary()) throw new Error("The active ChatGPT response left Temporary Chat");
        allowConnectorOnce();
        sendNative({ type: "responses-tool-state" });
        const state = window.__web2codexToolState;
        if (state && !state.active) return;
        const completedTools = state?.completed_tools || 0;
        const pendingTools = state?.pending_tools || 0;
        const completedToolBoundary = state && lastToolState &&
          completedTools > lastToolState.completed_tools;
        const nextToolBoundary = state && lastToolState && lastToolState.pending_tools === 0 &&
          pendingTools > 0 && completedTools > 0;
        // A tool-state boundary confirms the previous non-generating prose
        // was yielded before the tool round. It can appear for only one poll,
        // so do not require a second identical DOM snapshot here.
        if ((completedToolBoundary || nextToolBoundary) && !lastObservedGenerating && lastObservedRawText &&
            !isToolActivityPayload(lastObservedRawText) &&
            (!request.mcp_required || state.connector_requests > 0) &&
            (!request.mcp_action_required || state.tool_invocations > 0)) {
          preservedAssistantText = appendTranscriptText(preservedAssistantText, lastObservedRawText);
          for (const entry of lastObservedMessages) {
            if (!entry.previous && entry.text.trim()) preservedAssistantMessageRefs.add(entry.message);
          }
        }
        if (state) lastToolState = { pending_tools: pendingTools, completed_tools: completedTools };
        if (state && state.revision !== revision) { revision = state.revision; lastChange = Date.now(); }
        const previous = text;
        const messages = responseMessages(baseline);
        const submittedUser = userSnapshot().filter(entry => entry.id && !userBaseline.some(old => messageMatches(entry, old))).at(-1)?.message;
        rawText = responseText(messages, baseline, submittedUser);
        text = withPreservedTranscript(preservedAssistantText, rawText);
        if (text !== previous) {
          lastChange = Date.now();
        }
        const generating = stopVisible();
        lastObservedRawText = rawText;
        lastObservedMessages = messages;
        if (generating !== lastObservedGenerating) lastChange = Date.now();
        lastObservedGenerating = generating;
        const observedHeading = reasoningHeading(messages);
        if (observedHeading !== candidateHeading) {
          candidateHeading = observedHeading;
          candidateHeadingSince = Date.now();
          lastChange = candidateHeadingSince;
        }
        if (candidateHeading && candidateHeading !== sentHeading && (!generating || Date.now() - candidateHeadingSince >= 1000)) {
          sendNative({ type: "responses-reasoning-heading", heading: candidateHeading });
          sentHeading = candidateHeading;
        }
        if (text || generating || (state && (state.pending_tools > 0 || state.completed_tools > 0))) responseStarted = true;
        if (!responseStarted && Date.now() >= startDeadline) throw new Error("ChatGPT did not start responding within two minutes. Check the Web2Codex window and connector; the prompt was not retried.");
        const idle = Date.now() - lastChange;
        const approval = approvalVisible();
        if (approval || pendingTools > 0) lastChange = Date.now();
        const settled = state && state.pending_tools === 0 && !generating && !approval;
        const mcpObserved = !request.mcp_required || (state && state.connector_requests > 0);
        const mcpActionObserved = !request.mcp_action_required || (state && state.tool_invocations > 0);
        if (request.mcp_required && !mcpObserved && Date.now() >= startDeadline) {
          throw new Error("No MCP connector request was observed within two minutes; ChatGPT's response is unverified and was not shown as a completed result.");
        }
        if (request.mcp_action_required && !mcpActionObserved && Date.now() >= startDeadline) {
          throw new Error("No MCP tool action was observed within two minutes; discovery alone does not verify the requested operation, and no success result was shown.");
        }
        // Tool and subagent turns can rewrite the visible draft after a tool
        // round. Hold their answer until both the page and MCP activity settle.
        const stableToolAnswer = settled && state.quiet_ms >= 1200 && idle >= 1200;
        const toolWorkObserved = request.tool_work_expected || Boolean(
          state && (state.pending_tools > 0 || state.completed_tools > 0),
        );
        const canStream = mcpObserved && mcpActionObserved &&
          (!toolWorkObserved || stableToolAnswer || preservedAssistantText.length > streamedText.length);
        const hasDistinctFinalMessage = messages.some(({ message, text: messageText, previous }) =>
          !previous && messageText.trim() && !preservedAssistantMessageRefs.has(message),
        );
        const hasFinalText = Boolean(rawText.trim()) && (
          rawText.trim() !== preservedAssistantText.trim() ||
          (Boolean(preservedAssistantText.trim()) && hasDistinctFinalMessage)
        );
        const noTextTimeout = toolWorkObserved
          ? POST_TOOL_IDLE_TIMEOUT_MS
          : NO_RESPONSE_IDLE_TIMEOUT_MS;
        const generationTimeout = toolWorkObserved ? POST_TOOL_IDLE_TIMEOUT_MS : GENERATION_IDLE_TIMEOUT_MS;
        if (generating && !approval && state && pendingTools === 0 &&
            state.quiet_ms >= generationTimeout && idle >= generationTimeout) {
          throw new Error("ChatGPT stopped making observable progress while generating. Use /web to inspect the web session; the prompt and its tool calls were not retried.");
        }
        if (responseStarted && settled && !hasFinalText &&
            state.quiet_ms >= noTextTimeout && idle >= noTextTimeout) {
          if (state.completed_tools > 0) {
            throw new Error("ChatGPT finished its tool calls but returned no readable final answer. Check the tool activity above; the calls were not retried.");
          }
          throw new Error("ChatGPT stopped without readable response text. Use /web to inspect the web session before sending another request.");
        }
        if (request.mcp_required && !mcpObserved && settled && hasFinalText &&
            state.quiet_ms >= 1200 && idle >= 1200) {
          throw new Error("No MCP connector request was observed; ChatGPT's response is unverified and was not shown as a completed result.");
        }
        if (request.mcp_action_required && !mcpActionObserved && settled && hasFinalText &&
            state.quiet_ms >= 1200 && idle >= 1200) {
          throw new Error("No MCP tool action was observed; discovery alone does not verify the requested operation, and no success result was shown.");
        }
        const streamableText = toolWorkObserved && !stableToolAnswer
          ? preservedAssistantText
          : text;
        if (canStream && streamableText.startsWith(streamedText)) {
          const end = streamableText === preservedAssistantText
            ? streamableText.length
            : settled && text === previous && idle >= 1000
              ? text.length : stableEnd(previous, text, streamedText.length);
          if (end > streamedText.length) {
            sendNative({ type: "responses-text", delta: streamableText.slice(streamedText.length, end) });
            streamedText = streamableText.slice(0, end);
          }
        } else if (canStream && settled && state.quiet_ms >= 5000 && idle >= 5000 && hasFinalText) {
          throw new Error("ChatGPT changed previously streamed text. The prompt and its tool calls were not retried.");
        }
        if (canStream && settled && hasFinalText && text.startsWith(streamedText) && state.quiet_ms >= 1200 && idle >= 1200) {
          sendNative({ type: "responses-result", text, tool_revision: revision });
        }
        await sleep(100);
      }
    } catch (error) {
      if (!promptSubmitted) {
        const current = findComposer();
        if (current) {
          try { replaceText(current, ""); } catch {}
        }
      }
      if (!requestCancelled && !window.__web2codexAuthRequiredNotified) {
        sendNative({ type: "responses-result", error: terminalSafeText(String(error?.message || error)) });
      }
    } finally {
      busy = false;
      promptSubmitted = false;
      requestCancelled = false;
      activeRequestPrompt = "";
    }
  };

  (async () => {
    await waitForComposerOrLogin(60000, "ChatGPT did not expose a signed-in composer. Use /web login in the terminal to restore the web session", () => bindingRevision === 0);
    const probe = window.__web2codexCapabilityProbeComplete;
    if (probe?.then) await Promise.race([probe, sleep(45000)]);
    if (bindingRevision !== 0) return;
    sendNative({ type: "responses-bridge-ready", conversation_has_history: assistantMessages().length > 0 });
  })().catch(error => {
    if (bindingRevision !== 0) return;
    if (!window.__web2codexAuthRequiredNotified) {
      sendNative({ type: "responses-result", error: terminalSafeText(String(error?.message || error)) });
    }
  });
})();
