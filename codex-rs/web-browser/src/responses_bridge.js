(() => {
  if (window.top !== window) return;
  const requestId = window.__web2codexRequestId;
  const connectorName = "Web2Codex";
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
  const normalize = text => text.toLowerCase().replace(/[^a-z0-9]/g, "");
  const findComposer = () => {
    const matches = [...document.querySelectorAll('#prompt-textarea, [data-testid="prompt-textarea"], [contenteditable="true"][data-lexical-editor="true"], form[data-chatgpt-composer] [data-composer-markdown][contenteditable="true"][role="textbox"]')].filter(visible);
    return matches.length === 1 ? matches[0] : null;
  };
  const isTemporary = () => location.origin === "https://chatgpt.com" && location.pathname === "/" && new URL(location.href).searchParams.get("temporary-chat") === "true";
  const waitFor = async (read, timeout, error) => {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
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
  const connectorSelected = composer => [...composer.querySelectorAll('[data-id^="plugin:"][data-keyword], [app-mention-path^="app://"][app-mention-display-name][contenteditable="false"]')]
    .some(pill => visible(pill) && (pill.getAttribute("data-keyword") || pill.getAttribute("app-mention-display-name")) === connectorName);

  // The same @ menu and pill contract used by chatgpt-web-coder. A literal
  // @name in text does not count as selecting a connector.
  const attachConnector = async composer => {
    if (connectorSelected(composer)) return composer;
    replaceText(composer, "");
    for (const character of `@${connectorName}`) {
      replaceText(composer, character, true);
      await sleep(40);
    }
    const row = await waitFor(() => {
      const rows = [...document.querySelectorAll('.__menu-item[tabindex="0"], [data-mention-list-scroll-area] button[data-list-navigation-item="true"], [role="option"], [role="menuitem"]')]
        .filter(visible).filter(row => label(row) === connectorName || [...row.querySelectorAll("span, div")].some(child => visible(child) && label(child) === connectorName));
      const outer = rows.filter(row => !rows.some(other => other !== row && other.contains(row)));
      return outer.length === 1 ? outer[0] : null;
    }, 7000, `ChatGPT did not expose one connector named ${connectorName} in the @ menu. Add that custom MCP connector and enable it for this Temporary Chat.`);
    row.click();
    await waitFor(() => {
      allowConnectorOnce();
      const current = findComposer();
      return current && connectorSelected(current) ? current : null;
    }, 7000, `ChatGPT did not attach the ${connectorName} connector; the prompt was not sent`);
    return findComposer();
  };

  const ensurePersonalizedTemporaryChat = async () => {
    const unpersonalized = [...document.querySelectorAll("button")].find(button => visible(button) && /^(Unpersonalized|Без персонализации|Неперсонализированный|非个性化)$/i.test(label(button)));
    if (!unpersonalized) return;
    unpersonalized.click();
    const personalized = await waitFor(() => [...document.querySelectorAll('[role="dialog"] button, [role="menuitem"], [role="option"], [role="dialog"] [role="button"]')]
      .find(option => visible(option) && /^(Personalized|Personalize|С персонализацией|Персонализированный|个性化)$/i.test(label(option))), 4000, "Choose Personalized for Temporary Chats so the custom MCP connector is available");
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

  const attachFiles = async (composer, attachments) => {
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
      const cards = [...form.querySelectorAll('[role="group"], .composer-attachment-surface[aria-label]')];
      return attachments.every(attachment => cards.some(card => visible(card) && label(card) === attachment.name && !card.querySelector('[aria-busy="true"], [data-state="loading"], .animate-spin')));
    }, 60000, "ChatGPT did not confirm the file attachments; the prompt was not sent");
    const current = findComposer();
    if (!current || !connectorSelected(current)) throw new Error("The connector disappeared while attaching images; the prompt was not sent");
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
  const stopVisible = () => [...document.querySelectorAll("button")].some(button => visible(button) && (
    /stop-button|stop-generating/.test(button.getAttribute("data-testid") || "") ||
    /^(Stop(?: generating| streaming| responding| response)?|Остановить(?: генерацию| ответ)?)/i.test(label(button))));
  const approvalVisible = () => [...document.querySelectorAll('[role="dialog"], [data-message-author-role="assistant"] button')]
    .some(control => visible(control) && /^(Allow once|Allow|Always allow|Разрешить|Всегда разрешать)/i.test(label(control)));
  const assistantMessages = () => [...document.querySelectorAll('[data-message-author-role="assistant"], [data-message-author="assistant"]')].filter(visible);
  const responseText = baseline => assistantMessages().slice(baseline).map(message => {
    const markdown = [...message.querySelectorAll('.markdown, [data-markdown-text-style="assistant-message"], .puik-root.not-markdown > [class*="_DilResponseRoot"]')].filter(visible);
    return (markdown.length ? markdown.filter(node => !markdown.some(other => other !== node && other.contains(node))).map(node => node.innerText).join("\n\n") : message.innerText).trim();
  }).filter(Boolean).join("\n\n");

  let busy = false;
  window.__web2codexSend = async request => {
    if (busy || request.request_id !== requestId) return;
    busy = true;
    window.__web2codexCompleted = false;
    try {
      let composer = await waitFor(findComposer, 30000, "Sign in to ChatGPT before sending a Web2Codex request");
      await ensurePersonalizedTemporaryChat();
      await ensureRequestedModel(request.model);
      await ensureRequestedEffort(request.reasoning_effort);
      // Model and reasoning menus can re-render the composer. Never type into
      // the element captured before those selections.
      composer = await waitFor(findComposer, 10000, "ChatGPT composer did not return after selecting the model");
      composer = await attachConnector(composer);
      const baseline = assistantMessages().length;
      replaceText(composer, request.prompt, true);
      if (!connectorSelected(composer)) throw new Error("The connector pill disappeared; the prompt was not sent");
      const tail = request.prompt.replace(/\s+/g, " ").trim().slice(-120);
      if (!(composer.innerText || composer.value || "").replace(/\s+/g, " ").includes(tail)) throw new Error("ChatGPT did not preserve the prompt text");
      composer = await attachFiles(composer, request.attachments);
      const submit = await waitFor(() => {
        const form = findComposer()?.closest("form");
        return [...(form || document).querySelectorAll('button[data-testid="send-button"], button[data-testid="composer-send-button"], button[type="submit"]')]
          .find(button => visible(button) && !button.disabled);
      }, 5000, "ChatGPT send button stayed unavailable");
      submit.click();

      let text = "";
      let streamedText = "";
      let lastChange = Date.now();
      let revision = -1;
      const startDeadline = Date.now() + 120000;
      let responseStarted = false;
      // Tool rounds and approvals can outlive a single HTTP response. Observe
      // this same ChatGPT response until the native completion fence accepts it.
      while (!window.__web2codexCompleted) {
        if (!isTemporary()) throw new Error("The active ChatGPT response left Temporary Chat");
        allowConnectorOnce();
        sendNative({ type: "responses-tool-state" });
        const state = window.__web2codexToolState;
        if (state && !state.active) return;
        if (state && state.revision !== revision) { revision = state.revision; lastChange = Date.now(); }
        const current = responseText(baseline);
        if (current !== text) {
          text = current;
          lastChange = Date.now();
          if (!current.startsWith(streamedText)) throw new Error("ChatGPT revised text that was already streamed; the response was stopped to avoid inconsistent output");
          const delta = current.slice(streamedText.length);
          if (delta) {
            streamedText = current;
            sendNative({ type: "responses-text", delta });
          }
        }
        if (current || stopVisible() || (state && (state.pending_tools > 0 || state.completed_tools > 0))) responseStarted = true;
        if (!responseStarted && Date.now() >= startDeadline) throw new Error("ChatGPT did not start responding within two minutes. Check the Web2Codex window and connector; the prompt was not retried.");
        if (state && (text || state.completed_tools > 0) && state.pending_tools === 0 && state.quiet_ms >= 2500 && Date.now() - lastChange >= 2500 && !stopVisible() && !approvalVisible()) {
          sendNative({ type: "responses-result", text, tool_revision: revision });
        }
        await sleep(250);
      }
    } catch (error) {
      sendNative({ type: "responses-result", error: String(error?.message || error) });
    } finally { busy = false; }
  };

  (async () => {
    await waitFor(findComposer, 60000, "ChatGPT did not expose a signed-in composer");
    const probe = window.__web2codexCapabilityProbeComplete;
    if (probe?.then) await Promise.race([probe, sleep(45000)]);
    sendNative({ type: "responses-bridge-ready" });
  })().catch(error => sendNative({ type: "responses-result", error: String(error?.message || error) }));
})();
