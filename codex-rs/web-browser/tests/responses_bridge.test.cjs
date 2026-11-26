const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

test("a request applies native reasoning effort before submitting and streams terminal-safe text", async () => {
  let now = 0;
  let submitted = false;
  let attached = false;
  let uploadedNames = [];
  let generating = false;
  let stopClicks = 0;
  let firstTextDeltaWhileGenerating = false;
  const flow = [];
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const pill = makeElement({ getAttribute: name => name === "data-keyword" ? "Example Connector" : null });
  const input = makeElement({
    getAttribute: name => name === "accept" ? "image/*,audio/*" : null,
    dispatchEvent(event) {
      if (event.type === "change") { uploadedNames = this.files.map(file => file.name); attached = true; }
    },
  });
  const submit = makeElement({ disabled: false, click() { flow.push("submit"); submitted = true; generating = true; } });
  const form = makeElement({
    querySelector: () => input,
    querySelectorAll(selector) {
      if (selector.includes('input[type="file"]')) return [input];
      if (selector.includes("role=\"group\"")) return attached ? uploadedNames.map(name => makeElement({ getAttribute: value => value === "aria-label" ? name : null })) : [];
      if (selector.includes("send-button")) return [submit];
      return [];
    },
  });
  const composer = () => makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
    querySelectorAll: () => attachedConnector ? [pill] : [],
  });
  let attachedConnector = false;
  const oldComposer = composer();
  let currentComposer = oldComposer;
  const unpersonalized = makeElement({
    getAttribute: name => name === "aria-label" ? "Temporary chat options" : null,
    innerText: "Unpersonalized",
    click() { flow.push("unpersonalized"); },
  });
  const personalized = makeElement({
    getAttribute: name => name === "aria-label" ? "Mode option" : null,
    innerText: "Personalized",
    click() { flow.push("personalized"); },
  });
  const row = makeElement({
    getAttribute: name => name === "aria-label" ? "Select app" : null,
    innerText: "Example Connector",
    click() { flow.push("connector"); attachedConnector = true; currentComposer.innerText = ""; },
    contains: () => false,
  });
  const progressiveAnswer = "The answer is 42 🙂. This reply is intentionally long enough to prove that the first words arrive while ChatGPT is still generating the rest of the paragraph.";
  const terminalProgressiveAnswer = progressiveAnswer.replace("42", "\x1b[31m42\x1b[0m");
  const responseSnapshots = [
    "The answer is ",
    "The answer is 42",
    progressiveAnswer.slice(0, 112),
    `${terminalProgressiveAnswer}\nSecond line\x1b]52;c;PRIVATE_OSC_MARKER\x07\u009d52;c;PRIVATE_C1_OSC\u009c\u0090PRIVATE_C1_DCS\x1b\\`,
    `${terminalProgressiveAnswer}\nSecond line\x1b]52;c;PRIVATE_OSC_MARKER\x07\u009d52;c;PRIVATE_C1_OSC\u009c\u0090PRIVATE_C1_DCS\x1b\\`,
  ];
  let responseSnapshotIndex = 0;
  const finalAnswer = `${progressiveAnswer}\nSecond line`;
  const markdown = makeElement({ innerText: "" });
  const reasoningToggle = makeElement({
    matches: selector => selector.includes("aria-expanded"),
    getAttribute: name => name === "aria-expanded" ? "false" : null,
    innerText: "\x1b[31mThought for 3 seconds\x1b[0m\nPrivate reasoning body must not be forwarded",
  });
  const assistant = makeElement({ querySelectorAll: selector => {
    if (selector.includes("aria-expanded")) return [reasoningToggle];
    if (selector.includes(".markdown")) {
      markdown.innerText = responseSnapshots[Math.min(responseSnapshotIndex++, responseSnapshots.length - 1)];
      if (responseSnapshotIndex > responseSnapshots.length + 1) generating = false;
    }
    return [markdown];
  } });
  const stopButton = makeElement({ innerText: "Stop generating", click() { stopClicks++; generating = false; } });
  const document = {
    querySelectorAll(selector) {
      if (selector === "button") return generating ? [stopButton] : [];
      if (selector === 'button, [role="button"]') return [unpersonalized, ...(generating ? [stopButton] : [])];
      if (selector === 'button, [role="button"], [role="menuitem"], [role="option"]') return [personalized];
      if (selector.startsWith("#prompt-textarea")) return [currentComposer];
      if (selector.includes('[data-list-navigation-item="true"]')) return [row];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") currentComposer.innerText = "";
      if (command === "insertText") currentComposer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "request-1", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = {
    async select() { oldComposer.isConnected = false; currentComposer = composer(); },
    async setEffort(effort) { flow.push(`effort:${effort}`); },
  };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-text" && generating) firstTextDeltaWhileGenerating = true;
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 3000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  class FakeFile { constructor(_parts, name, options) { this.name = name; this.type = options.type; } }
  class FakeTransfer {
    constructor() { this.files = []; this.items = { add: file => this.files.push(file) }; }
  }
  const context = vm.createContext({
    window, document, URL, location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, File: FakeFile, DataTransfer: FakeTransfer, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  const source = fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8");
  vm.runInContext(source, context);
  await window.__web2codexSend({
    request_id: "request-1", model: "GPT-6", reasoning_effort: "medium", prompt: "Inspect this media",
    connector_required: true,
    attachments: [
      { name: "web2codex-image-1.png", mime: "image/png", data_url: "data:image/png;base64,aGVsbG8=" },
      { name: "web2codex-audio-2.wav", mime: "audio/wav", data_url: "data:audio/wav;base64,aGVsbG8=" },
    ],
  });
  assert.equal(oldComposer.innerText, "");
  assert.deepEqual(uploadedNames, ["web2codex-image-1.png", "web2codex-audio-2.wav"]);
  assert.equal(submitted, true);
  assert.deepEqual(flow, ["unpersonalized", "personalized", "effort:medium", "connector", "submit"]);
  assert.ok(
    flow.indexOf("effort:medium") < flow.indexOf("submit"),
    "the native ChatGPT effort must be applied before the prompt is submitted",
  );
  assert.equal(result.text, finalAnswer);
  assert.ok(responseSnapshotIndex >= responseSnapshots.length + 1, "the bridge should tolerate a repeated final DOM snapshot");
  assert.equal(result.error, undefined);
  assert.equal(stopClicks, 0, "a normally completed answer must not click ChatGPT's Stop control");
  assert.equal(JSON.stringify(nativeEvents).includes("\u001b"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("\u009b"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("\u009d"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("\u009c"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_OSC_MARKER"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_C1_OSC"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_C1_DCS"), false);
  const textEventIndex = nativeEvents.findIndex(event => event.type === "responses-text");
  const resultEventIndex = nativeEvents.findIndex(event => event.type === "responses-result");
  assert.ok(textEventIndex >= 0 && textEventIndex < resultEventIndex);
  assert.equal(firstTextDeltaWhileGenerating, true, "long prose must reach Codex before ChatGPT finishes the response");
  const streamedText = nativeEvents
    .filter(event => event.type === "responses-text")
    .map(event => event.delta)
    .join("");
  assert.equal(streamedText, finalAnswer);
  const reasoningEvent = nativeEvents.find(event => event.type === "responses-reasoning-heading");
  assert.deepEqual(reasoningEvent, {
    type: "responses-reasoning-heading",
    request_id: "request-1",
    heading: "Thought for 3 seconds",
  });
  assert.equal(JSON.stringify(nativeEvents).includes("Private reasoning body"), false);
});

test("request-scoped cancellation clicks ChatGPT Stop and ignores unrelated request ids", async () => {
  let submitted = false;
  let generating = false;
  let stopClicks = 0;
  let wrongIdResult;
  let correctIdResult;
  const nativeEvents = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const stopButton = makeElement({
    innerText: "Stop generating",
    click() { stopClicks++; generating = false; },
  });
  const sendButton = makeElement({
    disabled: false,
    click() { submitted = true; generating = true; },
  });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector === "button") return generating ? [stopButton] : [];
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.includes('[data-message-author-role')) return [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, text) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += text;
      return true;
    },
  };
  const window = { __web2codexRequestId: "request-1" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-prompt-submitted") {
      wrongIdResult = window.__web2codexCancel("agent-request-2");
      correctIdResult = window.__web2codexCancel("request-1");
    }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent, Event: FakeEvent,
    Date, setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  const source = fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8");
  vm.runInContext(source, context);
  await window.__web2codexSend({ request_id: "request-1", model: "GPT-6", prompt: "Stop this request" });

  assert.equal(submitted, true);
  assert.equal(wrongIdResult, false);
  assert.equal(correctIdResult, true);
  assert.equal(stopClicks, 1);
  assert.equal(generating, false);
  assert.equal(nativeEvents.some(event => event.type === "responses-result"), false);

  submitted = false;
  composer.innerText = "";
  await window.__web2codexBindRequest("request-2");
  const cancelledBeforeSubmit = window.__web2codexSend({
    request_id: "request-2", model: "GPT-6", prompt: "Cancel before sending",
  });
  assert.equal(window.__web2codexCancel("request-2"), true);
  await cancelledBeforeSubmit;
  assert.equal(submitted, false, "cancellation during request preparation must prevent sending the prompt");
  assert.equal(stopClicks, 1, "pre-submit cancellation must not click ChatGPT's Stop control");
});

test("a retained ChatGPT turn is captured when its existing assistant node is reused", async () => {
  let now = 0;
  let visibleAnswer = "OLDER_REPLY";
  let submitted = false;
  let result;
  const events = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", { get() { return visibleAnswer; } });
  const historicalReasoning = makeElement({
    innerText: "Thought for 3 seconds",
    matches: selector => selector.includes("[role=\"button\"][aria-expanded]") || selector.includes("aria-expanded"),
    getAttribute: name => name === "aria-expanded" ? "false" : null,
  });
  const assistant = makeElement({
    childElementCount: 1,
    querySelectorAll(selector) {
      if (selector.includes(".markdown")) return [markdown];
      if (selector.includes("[data-testid*=")) return [historicalReasoning];
      if (selector.includes("button[aria-expanded]")) return [historicalReasoning];
      return [];
    },
  });
  markdown.parentElement = assistant;
  const sendButton = makeElement({
    disabled: false,
    click() { submitted = true; visibleAnswer = "Понял))"; },
  });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      // ChatGPT reuses this message node instead of appending another one.
      if (selector.startsWith("[data-message-author-role")) return [assistant];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, text) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += text;
      return true;
    },
  };
  const window = { __web2codexRequestId: "reused-assistant-node" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    events.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = {
        active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 5000,
      };
    }
    if (event.type === "responses-result") {
      result = event;
      window.__web2codexCompleted = true;
    }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "reused-assistant-node", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Respond briefly", connector_required: false, attachments: [],
  });

  assert.equal(submitted, true);
  assert.equal(result.text, "Понял))");
  assert.equal(events.filter(event => event.type === "responses-text").map(event => event.delta).join(""), "Понял))");
  assert.equal(events.some(event => event.type === "responses-reasoning-heading"), false,
    "reasoning headings from the previous assistant turn must not be replayed");
  assert.equal(result.error, undefined);
});

test("short assistant text inside generic wrappers is read without markdown classes", async () => {
  let now = 0;
  let submitted = false;
  const events = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const textNode = makeElement({
    nodeType: 1, tagName: "DIV", firstElementChild: null, childNodes: [], innerText: "Понял))",
  });
  const assistant = makeElement({
    tagName: "DIV", childElementCount: 1, firstElementChild: textNode,
    childNodes: [textNode],
    querySelectorAll: () => [],
  });
  textNode.parentElement = assistant;
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    tagName: "DIV", isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector === "button") return [];
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "generic-wrapper-reply" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    events.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 3000 };
    }
    if (event.type === "responses-result") window.__web2codexCompleted = true;
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent, Event: FakeEvent,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "generic-wrapper-reply", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Reply briefly", connector_required: false, attachments: [],
  });

  assert.equal(submitted, true);
  assert.equal(events.filter(event => event.type === "responses-result").at(-1).text, "Понял))");
});

test("a short answer survives a delayed tool-state snapshot and streams before ChatGPT finishes", async () => {
  let now = 0;
  let submitted = false;
  let generating = false;
  let firstDeltaWhileGenerating = false;
  let snapshotCount = 0;
  let toolStatePolls = 0;
  const answer = "READY TO WORK";
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; generating = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", { get() { snapshotCount++; return answer; } });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  const stopButton = makeElement({ innerText: "Stop generating" });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return generating ? [stopButton] : [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "short-answer", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state" && ++toolStatePolls >= 2) {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 3000 };
    }
    if (event.type === "responses-text" && generating) {
      firstDeltaWhileGenerating = true;
      generating = false;
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL, location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  const source = fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8");
  vm.runInContext(source, context);
  await window.__web2codexSend({
    request_id: "short-answer", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Reply with exactly: READY TO WORK", connector_required: false, attachments: [],
  });

  const streamedText = nativeEvents
    .filter(event => event.type === "responses-text")
    .map(event => event.delta)
    .join("");
  assert.equal(submitted, true);
  assert.ok(toolStatePolls >= 2, "the bridge should tolerate a missing initial tool-state snapshot");
  assert.ok(snapshotCount >= 2, "the same short answer should be confirmed in multiple DOM snapshots");
  assert.equal(firstDeltaWhileGenerating, true, "short prose must reach Codex before ChatGPT finishes");
  assert.equal(streamedText, answer);
  assert.equal(result.text, answer);
  assert.equal(result.error, undefined);
});

test("a rewritten streamed prefix stops cleanly without resending the prompt", async () => {
  let now = 0;
  let submitted = false;
  let snapshotCount = 0;
  const initialAnswer = "The answer begins with a stable phrase that should reach the terminal before any rewrite";
  const rewrittenAnswer = "A different opening replaced the already-streamed response prefix";
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", {
    get() {
      snapshotCount++;
      return snapshotCount <= 2 ? initialAnswer : rewrittenAnswer;
    },
  });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "rewritten-prefix", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 10000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "rewritten-prefix", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Give a short answer", connector_required: false, attachments: [],
  });

  const text = nativeEvents.filter(event => event.type === "responses-text").map(event => event.delta).join("");
  const submittedEvents = nativeEvents.filter(event => event.type === "responses-prompt-submitted");
  assert.equal(submitted, true);
  assert.ok(text.length > 0, "the stable prefix should be streamed before the rewrite");
  assert.equal(text, initialAnswer.slice(0, text.length));
  assert.equal(submittedEvents.length, 1, "a changed response must never retry the user prompt");
  assert.equal(result.error, "ChatGPT changed previously streamed text. The prompt and its tool calls were not retried.");
  assert.equal(result.text, undefined);
});

test("a response with only controls ends with a readable-text error", async () => {
  let now = 0;
  let submitted = false;
  let stopChecks = 0;
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const stopButton = makeElement({ innerText: "Stop generating" });
  const assistant = makeElement({
    childElementCount: 1,
    innerText: "Copy response Retry",
    querySelectorAll: () => [],
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return stopChecks++ < 2 ? [stopButton] : [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "control-only-response", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 10000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "control-only-response", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Summarize the answer", connector_required: false, attachments: [],
  });

  assert.equal(submitted, true);
  assert.equal(nativeEvents.filter(event => event.type === "responses-text").length, 0);
  assert.equal(result.error, "ChatGPT stopped without readable response text. Use /web to inspect the web session before sending another request.");
  assert.equal(JSON.stringify(nativeEvents).includes("Copy response Retry"), false);
  assert.equal(result.text, undefined);
});

test("completed MCP tools without a final answer never produce an empty success", async () => {
  let now = 0;
  let submitted = false;
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const toolActivityOnly = makeElement({
    childElementCount: 1,
    innerText: 'Explored Read mcp-exec-smoke.txt {"arguments":{"path":"PRIVATE_TOOL_JSON"}}',
    querySelectorAll: () => [],
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [toolActivityOnly] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "tools-without-answer", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 1, pending_tools: 0, completed_tools: 2, quiet_ms: 1200000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "tools-without-answer", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Read a file and summarize it", connector_required: false, attachments: [],
  });

  assert.equal(submitted, true);
  assert.equal(nativeEvents.filter(event => event.type === "responses-text").length, 0);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_TOOL_JSON"), false);
  assert.equal(result.text, undefined);
  assert.equal(result.error, "ChatGPT finished its tool calls but returned no readable final answer. Check the tool activity above; the calls were not retried.");
});

test("a gap after a file tool does not abort before the requested subagent returns its summary", async () => {
  let now = 0;
  let submitted = false;
  let statePolls = 0;
  const subagentWaitPolls = 360;
  const nativeEvents = [];
  let result;
  const answer = "Прочитал файл через MCP; субагент прочитал тот же файл, результаты совпали.";
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", {
    get() { return statePolls >= subagentWaitPolls ? answer : ""; },
  });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted && statePolls >= subagentWaitPolls ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "subagent-final-summary", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      statePolls++;
      if (statePolls < subagentWaitPolls) {
        window.__web2codexToolState = {
          active: true,
          revision: 1,
          pending_tools: 0,
          completed_tools: 1,
          quiet_ms: statePolls * 1000,
        };
      } else {
        window.__web2codexToolState = {
          active: true,
          revision: 2,
          pending_tools: 0,
          completed_tools: 2,
          quiet_ms: 5000,
        };
      }
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "subagent-final-summary", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Read the file through MCP, wait for the subagent, and summarize the result",
    connector_required: false, attachments: [],
  });

  assert.equal(submitted, true);
  assert.ok(statePolls >= subagentWaitPolls, "the bridge must leave time for a subagent to finish before returning its summary");
  assert.equal(result.error, undefined);
  assert.equal(result.text, answer);
  assert.equal(nativeEvents.filter(event => event.type === "responses-result").length, 1);
});

test("fallback extraction keeps prose and card text but excludes reasoning and control leaves", async () => {
  let now = 0;
  let submitted = false;
  const nativeEvents = [];
  let result;
  const makeElement = (tagName, extra = {}) => ({
    tagName, isConnected: true, parentElement: null, childNodes: [], childElementCount: 0,
    firstElementChild: null, innerText: "", textContent: "",
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    matches(selector) {
      if (selector.includes("[data-testid*='reason'")) return false;
      if (selector.includes('[data-testid*="reason"')) return this.getAttribute("data-testid")?.toLowerCase().includes("reason") || false;
      if (selector.includes("[aria-label*=") && /reason|think|thought/i.test(this.getAttribute("aria-label") || "")) return true;
      if (selector.includes('[role="dialog"]') && this.getAttribute("role") === "dialog") return true;
      if (selector.includes("h1,") && /^H[1-6]$/.test(this.tagName)) return true;
      if (selector.includes("button") && this.tagName === "BUTTON") return true;
      if (selector.includes("[hidden]")) return false;
      return false;
    },
    ...extra,
  });
  const answer = makeElement("P", { innerText: "Ответ с emoji 🙂." });
  const docsLink = makeElement("A", {
    nodeType: 1,
    innerText: "OpenAI API docs",
    textContent: "OpenAI API docs",
    getAttribute: name => name === "href" ? "https://platform.openai.com/docs" : null,
  });
  const docsParagraph = makeElement("P", {
    childNodes: [docsLink],
    firstElementChild: docsLink,
    childElementCount: 1,
  });
  docsLink.parentElement = docsParagraph;
  const unsafeLink = makeElement("A", {
    nodeType: 1,
    innerText: "Unsafe link label",
    textContent: "Unsafe link label",
    getAttribute: name => name === "href" ? "javascript:alert(1)" : null,
  });
  const unsafeLinkParagraph = makeElement("P", {
    childNodes: [unsafeLink],
    firstElementChild: unsafeLink,
    childElementCount: 1,
  });
  unsafeLink.parentElement = unsafeLinkParagraph;
  const reasoningBody = makeElement("P", { innerText: "PRIVATE_REASONING_BODY_MARKER" });
  const heading = makeElement("H2", { innerText: "Thought for 3 seconds", textContent: "Thought for 3 seconds" });
  const sectionHeading = makeElement("H3", { innerText: "Файлы", textContent: "Файлы" });
  const cardAction = makeElement("BUTTON", { nodeType: 1, innerText: "OPEN_CARD_ACTION" });
  const cardMetadata = makeElement("SPAN", {
    nodeType: 1,
    innerText: "PRIVATE_CARD_METADATA",
    matches: selector => selector.includes('[aria-hidden="true"]'),
  });
  const cardEmphasis = makeElement("STRONG", { nodeType: 1, innerText: "test", textContent: "test" });
  const cardHeading = makeElement("H4", { innerText: "Deployment card", textContent: "Deployment card" });
  const cardBody = makeElement("P", {
    childNodes: [{ nodeType: 3, nodeValue: "Build → " }, cardEmphasis, { nodeType: 3, nodeValue: ". " }, cardAction, cardMetadata],
    innerText: "Build → test. OPEN_CARD_ACTION PRIVATE_CARD_METADATA",
    firstElementChild: cardEmphasis,
    childElementCount: 3,
  });
  const responseCard = makeElement("DIV", {
    children: [cardHeading, cardBody],
    childNodes: [cardHeading, cardBody],
    childElementCount: 2,
    firstElementChild: cardHeading,
  });
  cardAction.parentElement = cardBody;
  cardMetadata.parentElement = cardBody;
  cardEmphasis.parentElement = cardBody;
  cardHeading.parentElement = responseCard;
  cardBody.parentElement = responseCard;
  const listItem = makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: "README.md" }],
    innerText: "README.md",
  });
  const secondListItem = makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: "src/" }],
    innerText: "src/",
  });
  const fileList = makeElement("UL", {
    children: [listItem, secondListItem],
    childNodes: [listItem, secondListItem],
    childElementCount: 2,
    firstElementChild: listItem,
  });
  listItem.parentElement = fileList;
  secondListItem.parentElement = fileList;
  const orderedFour = makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: "Step four" }],
    innerText: "Step four",
  });
  const orderedSeven = makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: "Step seven" }],
    innerText: "Step seven",
    getAttribute: name => name === "value" ? "7" : null,
  });
  const nestedBullet = makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: "Nested child" }],
    innerText: "Nested child",
  });
  const nestedList = makeElement("UL", {
    children: [nestedBullet],
    childNodes: [nestedBullet],
    childElementCount: 1,
    firstElementChild: nestedBullet,
  });
  nestedList.nodeType = 1;
  const orderedParent = makeElement("LI", {
    children: [nestedList],
    childNodes: [{ nodeType: 3, nodeValue: "Parent" }, nestedList],
    childElementCount: 1,
    firstElementChild: nestedList,
    innerText: "Parent\nNested child",
  });
  const orderedList = makeElement("OL", {
    children: [orderedFour, orderedSeven, orderedParent],
    childNodes: [orderedFour, orderedSeven, orderedParent],
    childElementCount: 3,
    firstElementChild: orderedFour,
    getAttribute: name => name === "start" ? "4" : null,
  });
  orderedFour.parentElement = orderedList;
  orderedSeven.parentElement = orderedList;
  orderedParent.parentElement = orderedList;
  nestedBullet.parentElement = nestedList;
  nestedList.parentElement = orderedParent;
  const reversedItems = ["Newest", "Middle", "Oldest"].map(text => makeElement("LI", {
    childNodes: [{ nodeType: 3, nodeValue: text }],
    innerText: text,
  }));
  const reversedList = makeElement("OL", {
    children: reversedItems,
    childNodes: reversedItems,
    childElementCount: reversedItems.length,
    firstElementChild: reversedItems[0],
    getAttribute: name => name === "reversed" ? "" : null,
  });
  for (const item of reversedItems) item.parentElement = reversedList;
  const packageHeader = makeElement("TH", { innerText: "Package" });
  const statusHeader = makeElement("TH", { innerText: "Status" });
  const packageValue = makeElement("TD", { innerText: "Web2Codex" });
  const statusValue = makeElement("TD", { innerText: "Ready" });
  const hiddenValue = makeElement("TD", {
    innerText: "Hidden cell",
    matches: selector => selector.includes("[hidden]"),
  });
  const copyButton = makeElement("BUTTON", { nodeType: 1, innerText: "Copy" });
  const controlValue = makeElement("TD", {
    childNodes: [copyButton],
    firstElementChild: copyButton,
    childElementCount: 1,
  });
  const nestedValue = makeElement("TD", { innerText: "Nested table row" });
  const nestedRow = makeElement("TR", { querySelectorAll: () => [nestedValue] });
  const nestedTable = makeElement("TABLE", {
    querySelectorAll: selector => selector === "tr" ? [nestedRow] : [],
  });
  nestedTable.nodeType = 1;
  packageValue.childNodes = [{ nodeType: 3, nodeValue: "Web2Codex" }, nestedTable];
  packageValue.firstElementChild = nestedTable;
  packageValue.childElementCount = 1;
  const emptyRow = makeElement("TR", { querySelectorAll: () => [] });
  const headerRow = makeElement("TR", { querySelectorAll: () => [packageHeader, statusHeader] });
  const valueRow = makeElement("TR", {
    querySelectorAll: () => [packageValue, statusValue, hiddenValue, controlValue, nestedValue],
  });
  const responseTable = makeElement("TABLE", {
    querySelectorAll: selector => selector === "tr" ? [emptyRow, headerRow, valueRow, nestedRow] : [],
  });
  for (const cell of [packageHeader, statusHeader]) cell.parentElement = headerRow;
  for (const cell of [packageValue, statusValue, hiddenValue, controlValue]) cell.parentElement = valueRow;
  copyButton.parentElement = controlValue;
  emptyRow.parentElement = responseTable;
  headerRow.parentElement = responseTable;
  valueRow.parentElement = responseTable;
  nestedValue.parentElement = nestedRow;
  nestedRow.parentElement = nestedTable;
  nestedTable.parentElement = packageValue;
  const unlabelledFirst = makeElement("TD", { innerText: "No header" });
  const unlabelledSecond = makeElement("TD", { innerText: "42" });
  const unlabelledRow = makeElement("TR", { querySelectorAll: () => [unlabelledFirst, unlabelledSecond] });
  const unlabelledTable = makeElement("TABLE", {
    querySelectorAll: selector => selector === "tr" ? [unlabelledRow] : [],
  });
  unlabelledFirst.parentElement = unlabelledRow;
  unlabelledSecond.parentElement = unlabelledRow;
  unlabelledRow.parentElement = unlabelledTable;
  const mergedHeader = makeElement("TH", {
    innerText: "All details",
    getAttribute: name => name === "colspan" ? "2" : null,
  });
  const spanHeaderRow = makeElement("TR", { querySelectorAll: () => [mergedHeader] });
  const spanFirst = makeElement("TD", { innerText: "First" });
  const spanSecond = makeElement("TD", { innerText: "Second" });
  const spanValueRow = makeElement("TR", { querySelectorAll: () => [spanFirst, spanSecond] });
  const spanTable = makeElement("TABLE", {
    querySelectorAll: selector => selector === "tr" ? [spanHeaderRow, spanValueRow] : [],
  });
  mergedHeader.parentElement = spanHeaderRow;
  spanFirst.parentElement = spanValueRow;
  spanSecond.parentElement = spanValueRow;
  spanHeaderRow.parentElement = spanTable;
  spanValueRow.parentElement = spanTable;
  const quote = makeElement("BLOCKQUOTE", {
    childNodes: [{ nodeType: 3, nodeValue: "A short quoted note.\nSecond line." }],
  });
  const code = makeElement("CODE", { innerText: "npm test\nnode server.js" });
  const codeBlock = makeElement("PRE", {
    innerText: "npm test\nnode server.js Copy",
    querySelector: selector => selector === "code" ? code : null,
  });
  code.parentElement = codeBlock;
  const reasoningScope = makeElement("DIV", {
    getAttribute: name => name === "data-testid" ? "reasoning-summary" : null,
    querySelectorAll: selector => selector.startsWith("h1,") ? [heading] : [],
  });
  reasoningBody.parentElement = reasoningScope;
  heading.parentElement = reasoningScope;
  answer.parentElement = null;
  reasoningScope.parentElement = null;
  const message = makeElement("DIV", {
    childElementCount: 4,
    querySelectorAll(selector) {
      if (selector.includes(".markdown") || selector.includes("data-markdown-text-style")) return [];
      if (selector.startsWith("[data-testid*=")) return [reasoningScope];
      if (selector.startsWith("button[aria-expanded]")) return [];
      if (selector.startsWith("p, pre, ul, ol, blockquote, table")) return [reasoningBody, answer, docsParagraph, unsafeLinkParagraph, sectionHeading, cardHeading, cardBody, fileList, orderedList, reversedList, responseTable, unlabelledTable, spanTable, quote, codeBlock];
      return [];
    },
  });
  const reasoningLeaf = makeElement("DIV", {
    innerText: "PRIVATE_REASONING_LEAF_MARKER",
    getAttribute: name => name === "data-testid" ? "reasoning-summary" : null,
  });
  const controlLeaf = makeElement("DIV", {
    innerText: "PRIVATE_CONTROL_LEAF_MARKER",
    getAttribute: name => name === "role" ? "dialog" : null,
  });
  const standalone = makeElement("DIV", { innerText: "Standalone visible reply" });
  reasoningScope.parentElement = message;
  answer.parentElement = message;
  docsParagraph.parentElement = message;
  unsafeLinkParagraph.parentElement = message;
  sectionHeading.parentElement = message;
  responseCard.parentElement = message;
  fileList.parentElement = message;
  orderedList.parentElement = message;
  reversedList.parentElement = message;
  responseTable.parentElement = message;
  unlabelledTable.parentElement = message;
  spanTable.parentElement = message;
  quote.parentElement = message;
  codeBlock.parentElement = message;
  const sendButton = makeElement("BUTTON", { disabled: false, click() { submitted = true; } });
  const form = makeElement("FORM", { querySelectorAll: selector => selector.includes("send-button") ? [sendButton] : [] });
  const composer = makeElement("DIV", {
    isContentEditable: true,
    closest: () => form,
    focus() {}, dispatchEvent() {},
    querySelectorAll: () => [],
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [message, reasoningLeaf, controlLeaf, standalone] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "fallback-reasoning", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(serialized) {
    const event = JSON.parse(serialized);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 3000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent, Event: FakeEvent,
    Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "fallback-reasoning", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Summarize the answer", connector_required: false, attachments: [],
  });

  const textEvents = nativeEvents.filter(event => event.type === "responses-text");
  assert.equal(submitted, true);
  assert.equal(textEvents.map(event => event.delta).join(""), "Ответ с emoji 🙂.\n\nOpenAI API docs (https://platform.openai.com/docs)\n\nUnsafe link label\n\n### Файлы\n\n#### Deployment card\n\nBuild → test.\n\n- README.md\n- src/\n\n4. Step four\n7. Step seven\n8. Parent\n  - Nested child\n\n3. Newest\n2. Middle\n1. Oldest\n\n- Package: Web2Codex - Nested table row; Status: Ready\n\n- No header; 42\n\n- All details\n- First; Second\n\n> A short quoted note.\n> Second line.\n\n    npm test\n    node server.js\n\nStandalone visible reply");
  const displayedText = textEvents.map(event => event.delta).join("");
  assert.equal(displayedText.includes("\t"), false);
  assert.equal(displayedText.includes("javascript:"), false);
  assert.equal(textEvents.map(event => event.delta).join("").includes("Hidden cell"), false);
  assert.equal(displayedText.split("Nested table row").length - 1, 1);
  assert.equal(textEvents.map(event => event.delta).join("").includes("Copy"), false);
  assert.equal(displayedText.includes("OPEN_CARD_ACTION"), false);
  assert.equal(displayedText.includes("PRIVATE_CARD_METADATA"), false);
  assert.deepEqual(
    nativeEvents.filter(event => event.type === "responses-reasoning-heading").map(event => event.heading),
    ["Thought for 3 seconds"],
  );
  assert.equal(result.text, textEvents.map(event => event.delta).join(""));
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_REASONING_BODY_MARKER"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_REASONING_LEAF_MARKER"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_CONTROL_LEAF_MARKER"), false);
});

test("standard markdown containers flatten card text and omit action metadata", async () => {
  let now = 0;
  let submitted = false;
  const nativeEvents = [];
  let result;
  const makeElement = (tagName = "DIV", extra = {}) => ({
    nodeType: 1,
    tagName,
    isConnected: true,
    parentElement: null,
    childNodes: [],
    childElementCount: 0,
    firstElementChild: null,
    innerText: "",
    textContent: "",
    matches(selector) {
      if (selector.includes("button") && this.tagName === "BUTTON") return true;
      if (selector.includes("[hidden]") && this.getAttribute("hidden") !== null) return true;
      return selector.includes('[aria-hidden="true"]') && this.getAttribute("aria-hidden") === "true";
    },
    getAttribute: () => null,
    querySelector: () => null,
    querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement("BUTTON", { disabled: false, click() { submitted = true; } });
  const form = makeElement("FORM", {
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement("DIV", {
    isContentEditable: true,
    closest: () => form,
    focus() {},
    dispatchEvent() {},
  });
  const action = makeElement("BUTTON", { innerText: "OPEN_CARD_ACTION" });
  const privateMetadata = makeElement("SPAN", {
    innerText: "PRIVATE_CARD_METADATA",
    getAttribute: name => name === "aria-hidden" ? "true" : null,
  });
  const emphasis = makeElement("STRONG", { innerText: "test", textContent: "test" });
  const paragraph = makeElement("P", {
    childNodes: [
      { nodeType: 3, nodeValue: "Build → " },
      emphasis,
      { nodeType: 3, nodeValue: ". " },
      action,
      privateMetadata,
    ],
    firstElementChild: emphasis,
    childElementCount: 3,
  });
  const chartSummary = makeElement("P", { innerText: "Chart: sales ↑", textContent: "Chart: sales ↑" });
  const chartPath = makeElement("PATH", {
    innerText: "",
    textContent: "",
    getAttribute: name => name === "d" ? "PRIVATE_CHART_PATH_DATA" : null,
  });
  const chart = makeElement("SVG", { childNodes: [chartPath], firstElementChild: chartPath, childElementCount: 1 });
  const chartAction = makeElement("BUTTON", { innerText: "OPEN_CHART_ACTION" });
  const chartBlock = makeElement("DIV", {
    childNodes: [chartSummary, chart, chartAction],
    firstElementChild: chartSummary,
    childElementCount: 3,
  });
  const stageHeader = makeElement("TH", { innerText: "Stage" });
  const statusHeader = makeElement("TH", { innerText: "Status" });
  const headerRow = makeElement("TR", {
    querySelectorAll(selector) { return selector === "th, td" ? [stageHeader, statusHeader] : []; },
  });
  const stageValue = makeElement("TD", { innerText: "Discovery" });
  const statusValue = makeElement("TD", { innerText: "Connected" });
  const valueRow = makeElement("TR", {
    querySelectorAll(selector) { return selector === "th, td" ? [stageValue, statusValue] : []; },
  });
  const table = makeElement("TABLE", {
    querySelectorAll(selector) { return selector === "tr" ? [headerRow, valueRow] : []; },
  });
  const heading = makeElement("H4", { innerText: "Deployment card", textContent: "Deployment card" });
  const card = makeElement("DIV", {
    childNodes: [heading, paragraph, chartBlock, table],
    firstElementChild: heading,
    childElementCount: 4,
  });
  const markdown = makeElement("DIV", {
    childNodes: [card],
    firstElementChild: card,
    childElementCount: 1,
  });
  const assistant = makeElement("DIV", {
    childElementCount: 1,
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  action.parentElement = paragraph;
  privateMetadata.parentElement = paragraph;
  emphasis.parentElement = paragraph;
  chartSummary.parentElement = chartBlock;
  chartPath.parentElement = chart;
  chart.parentElement = chartBlock;
  chartAction.parentElement = chartBlock;
  chartBlock.parentElement = card;
  stageHeader.parentElement = headerRow;
  statusHeader.parentElement = headerRow;
  stageValue.parentElement = valueRow;
  statusValue.parentElement = valueRow;
  headerRow.parentElement = table;
  valueRow.parentElement = table;
  table.parentElement = card;
  heading.parentElement = card;
  paragraph.parentElement = card;
  card.parentElement = markdown;
  markdown.parentElement = assistant;
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "markdown-card", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 3000 };
    }
    if (event.type === "responses-result") { result = event; window.__web2codexCompleted = true; }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "markdown-card", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Summarize this deployment card", connector_required: false, attachments: [],
  });

  const streamedText = nativeEvents.filter(event => event.type === "responses-text").map(event => event.delta).join("");
  assert.equal(submitted, true);
  assert.equal(streamedText, "#### Deployment card\nBuild → test.\nChart: sales ↑\n- Stage: Discovery; Status: Connected");
  assert.equal(result.text, streamedText);
  assert.equal(JSON.stringify(nativeEvents).includes("OPEN_CARD_ACTION"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("OPEN_CHART_ACTION"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_CARD_METADATA"), false);
  assert.equal(JSON.stringify(nativeEvents).includes("PRIVATE_CHART_PATH_DATA"), false);
});

test("a near-match connector is ignored without leaving the query or sending the user prompt", async () => {
  let now = 0;
  let submitted = false;
  let selectedWrongConnector = false;
  let result;
  const nativeEvents = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true,
    innerText: "",
    focus() {},
    dispatchEvent() {},
    closest: () => form,
  });
  const nearMatch = makeElement({
    innerText: "Example Connector",
    click() { selectedWrongConnector = true; },
    contains: () => false,
  });
  const connectorName = "Cosmic-Birch\x1b]52;c;PRIVATE_ERROR_OSC\x07\u009b31m";
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.includes('[data-list-navigation-item="true"]')) return [nearMatch];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "near-match", __web2codexConnectorName: connectorName };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-result") result = event;
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL, location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  const source = fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8");
  vm.runInContext(source, context);
  await window.__web2codexSend({
    request_id: "near-match", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Do not leak this prompt", connector_required: true, attachments: [],
  });

  assert.equal(submitted, false);
  assert.equal(selectedWrongConnector, false);
  assert.ok(result.error.includes("ChatGPT did not show the configured connector Cosmic-Birch in the @ menu"));
  assert.equal(result.error.includes("\u001b"), false);
  assert.equal(result.error.includes("\u009b"), false);
  assert.equal(result.error.includes("PRIVATE_ERROR_OSC"), false);
  assert.equal(composer.innerText, "");
  assert.equal(composer.innerText.includes("Do not leak this prompt"), false);
  assert.equal(nativeEvents.some(event => event.type === "responses-prompt-submitted"), false);
  assert.equal(nativeEvents.some(event => event.type === "responses-text"), false);
});

test("a connector that disappears before submit clears the unsent user prompt", async () => {
  let now = 0;
  let pillPresent = false;
  let submitted = false;
  let result;
  const nativeEvents = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const pill = makeElement({ getAttribute: name => name === "data-keyword" ? "Example Connector" : null });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true,
    innerText: "",
    focus() {},
    dispatchEvent() {},
    closest: () => form,
    querySelectorAll: () => pillPresent ? [pill] : [],
  });
  const row = makeElement({
    innerText: "Example Connector",
    click() { pillPresent = true; },
    contains: () => false,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.includes('[data-list-navigation-item="true"]')) return [row];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") {
        composer.innerText += value;
        if (value === "Sensitive prompt text") pillPresent = false;
      }
      return true;
    },
  };
  const window = { __web2codexRequestId: "lost-connector", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-result") result = event;
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL, location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  const source = fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8");
  vm.runInContext(source, context);
  await window.__web2codexSend({
    request_id: "lost-connector", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Sensitive prompt text", connector_required: true, attachments: [],
  });

  assert.equal(submitted, false);
  assert.equal(composer.innerText, "");
  assert.ok(result.error.includes("connector pill disappeared"));
  assert.equal(nativeEvents.some(event => event.type === "responses-prompt-submitted"), false);
  assert.equal(nativeEvents.some(event => event.type === "responses-text"), false);
});

async function runMcpRequiredBridgeCase(connectorRequests, { generating = false, toolInvocations = 0, actionRequired = false } = {}) {
  let now = 0;
  let submitted = false;
  let pillPresent = false;
  let result;
  const nativeEvents = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const pill = makeElement({
    getAttribute(name) { return name === "data-keyword" ? "Example Connector" : null; },
  });
  const sendButton = makeElement({ disabled: false, innerText: "Send", click() { submitted = true; } });
  const stopButton = makeElement({ innerText: "Stop generating" });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true,
    innerText: "",
    focus() {},
    dispatchEvent() {},
    closest: () => form,
    querySelectorAll: () => pillPresent ? [pill] : [],
  });
  const answer = makeElement({ innerText: "MCP discovery succeeded: 29 tools." });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [answer] : []; },
  });
  const row = makeElement({
    innerText: "Example Connector",
    click() { pillPresent = true; composer.innerText = ""; },
    contains: () => false,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector.includes('[data-list-navigation-item="true"]')) return [row];
      if (selector === "button") return generating ? [sendButton, stopButton] : [sendButton];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "mcp-required", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
    if (event.type === "responses-tool-state") {
      window.__web2codexToolState = {
        active: true, revision: 1, pending_tools: 0, completed_tools: 0,
        connector_requests: connectorRequests, tool_invocations: toolInvocations, quiet_ms: 1200000,
      };
    }
    if (event.type === "responses-result") {
      result = event;
      window.__web2codexCompleted = true;
    }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "mcp-required", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Use only Example Connector MCP. Discover the tools.",
    connector_required: true, mcp_required: true, mcp_action_required: actionRequired, attachments: [],
  });
  return { result, nativeEvents, submitted };
}

test("an explicit MCP request with no connector traffic cannot stream a success claim", async () => {
  const { result, nativeEvents, submitted } = await runMcpRequiredBridgeCase(0);

  assert.equal(submitted, true);
  assert.equal(result.text, undefined);
  assert.match(result.error, /No MCP connector request was observed/);
  assert.equal(nativeEvents.filter(event => event.type === "responses-text").length, 0);
});

test("an explicit MCP request streams only after connector traffic is observed", async () => {
  const { result, nativeEvents, submitted } = await runMcpRequiredBridgeCase(1);

  assert.equal(submitted, true);
  assert.equal(result.error, undefined);
  assert.equal(result.text, "MCP discovery succeeded: 29 tools.");
  assert.ok(nativeEvents.some(event => event.type === "responses-text"));
});

test("MCP discovery alone cannot claim completion of a requested tool action", async () => {
  const { result, nativeEvents, submitted } = await runMcpRequiredBridgeCase(1, { actionRequired: true });

  assert.equal(submitted, true);
  assert.equal(result.text, undefined);
  assert.match(result.error, /No MCP tool action was observed/);
  assert.equal(nativeEvents.filter(event => event.type === "responses-text").length, 0);
});

test("an MCP action request streams only after a wire tool call is observed", async () => {
  const { result, nativeEvents, submitted } = await runMcpRequiredBridgeCase(2, {
    toolInvocations: 1,
    actionRequired: true,
  });

  assert.equal(submitted, true);
  assert.equal(result.error, undefined);
  assert.equal(result.text, "MCP discovery succeeded: 29 tools.");
  assert.ok(nativeEvents.some(event => event.type === "responses-text"));
});

test("an MCP-required turn fails clearly if ChatGPT keeps generating without calling the connector", async () => {
  const { result, nativeEvents, submitted } = await runMcpRequiredBridgeCase(0, { generating: true });

  assert.equal(submitted, true);
  assert.match(result.error, /No MCP connector request was observed within two minutes/);
  assert.equal(nativeEvents.filter(event => event.type === "responses-text").length, 0);
});

test("tool and subagent turns publish only the stable final answer", async () => {
  let now = 0;
  let submitted = false;
  let statePolls = 0;
  let snapshotReads = 0;
  const draftOne = 'Tool call {"wire_name":"functions.exec_command","arguments":{"path":"PRIVATE_MCP_ARGUMENTS"}}';
  const draftTwo = 'Tool result {"structuredContent":{"value":"PRIVATE_STRUCTURED_RESULT"}}';
  const answer = "All five agents finished. Their results match.";
  const events = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", {
    get() {
      snapshotReads++;
      if (statePolls <= 1) return draftOne;
      if (statePolls <= 3) return draftTwo;
      return answer;
    },
  });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, value) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += value;
      return true;
    },
  };
  const window = { __web2codexRequestId: "stable-tool-answer", __web2codexConnectorName: "Example Connector" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {}, async setEffort() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    events.push(event);
    if (event.type === "responses-tool-state") {
      statePolls++;
      const completion = statePolls <= 1
        ? { revision: 1, pending_tools: 5, completed_tools: 0 }
        : statePolls === 2
          ? { revision: 2, pending_tools: 4, completed_tools: 1 }
          : statePolls <= 3
            ? { revision: 3, pending_tools: 1, completed_tools: 4 }
            : { revision: 4, pending_tools: 0, completed_tools: 5 };
      window.__web2codexToolState = {
        active: true,
        ...completion,
        quiet_ms: 5000,
      };
    }
    if (event.type === "responses-result") {
      result = event;
      window.__web2codexCompleted = true;
    }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "stable-tool-answer", model: "GPT-6", reasoning_effort: "medium",
    prompt: "Run five agents in parallel and compare their results",
    // The tool use was not predicted from the prompt; once observed, its
    // mutable drafts must still remain hidden until the final answer settles.
    connector_required: false, mcp_required: false, tool_work_expected: false, attachments: [],
  });

  const streamed = events.filter(event => event.type === "responses-text").map(event => event.delta).join("");
  assert.equal(submitted, true);
  assert.ok(statePolls >= 4, "the response must wait for all five agent results");
  assert.ok(snapshotReads >= 4, "the draft should be observed changing before the final answer");
  assert.equal(streamed, answer, "mutable tool-turn drafts must never enter the append-only TUI stream");
  assert.equal(JSON.stringify(events).includes("PRIVATE_MCP_ARGUMENTS"), false);
  assert.equal(JSON.stringify(events).includes("PRIVATE_STRUCTURED_RESULT"), false);
  assert.equal(result.text, answer);
  assert.equal(result.error, undefined);
});

test("stable ChatGPT commentary between tool rounds survives when the page replaces its message", async () => {
  let now = 0;
  let submitted = false;
  let statePoll = 0;
  let visibleAssistantText = "";
  const commentary = [
    "The first agent found the main issue; I am checking it independently.",
    "Two results agree, so I am asking another agent to inspect the edge case.",
    "The last agent is checking the conflicting detail before I summarize.",
  ];
  const finalSummary = "All four agent results agree, and the edge case is safe.";
  const finalAnswer = `${commentary[0]}\n\n${finalSummary}`;
  const events = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const sendButton = makeElement({ disabled: false, click() { submitted = true; } });
  const form = makeElement({
    querySelectorAll(selector) { return selector.includes("send-button") ? [sendButton] : []; },
  });
  const composer = makeElement({
    isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {},
    closest: () => form,
  });
  const markdown = makeElement({});
  Object.defineProperty(markdown, "innerText", { get() { return visibleAssistantText; } });
  const assistant = makeElement({
    querySelectorAll(selector) { return selector.includes(".markdown") ? [markdown] : []; },
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return [];
      if (selector.startsWith("[data-message-author-role")) return submitted ? [assistant] : [];
      return [];
    },
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand(command, _ui, text) {
      if (command === "delete") composer.innerText = "";
      if (command === "insertText") composer.innerText += text;
      return true;
    },
  };
  const window = { __web2codexRequestId: "intermediate-commentary" };
  window.top = window;
  window.__web2codexModelPicker = { async select() {} };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    events.push(event);
    if (event.type === "responses-tool-state") {
      statePoll++;
      const states = [
        { revision: 1, pending_tools: 4, completed_tools: 0 },
        { revision: 2, pending_tools: 3, completed_tools: 1 },
        { revision: 2, pending_tools: 3, completed_tools: 1 },
        { revision: 3, pending_tools: 2, completed_tools: 2 },
        { revision: 3, pending_tools: 2, completed_tools: 2 },
        { revision: 4, pending_tools: 1, completed_tools: 3 },
        { revision: 4, pending_tools: 1, completed_tools: 3 },
        { revision: 5, pending_tools: 0, completed_tools: 4 },
        { revision: 5, pending_tools: 0, completed_tools: 4 },
      ];
      const contents = [
        "",
        commentary[0],
        commentary[0],
        'Tool call {"wire_name":"collaboration.spawn_agent","arguments":{"task":"PRIVATE_AGENT_TASK"}}',
        commentary[1],
        'Tool result {"structuredContent":{"secret":"PRIVATE_AGENT_RESULT"}}',
        commentary[2],
        finalAnswer,
        finalAnswer,
      ];
      window.__web2codexToolState = {
        active: true,
        connector_requests: 1,
        tool_invocations: 4,
        quiet_ms: 5000,
        ...states[Math.min(statePoll - 1, states.length - 1)],
      };
      visibleAssistantText = contents[Math.min(statePoll - 1, contents.length - 1)];
    }
    if (event.type === "responses-result") {
      result = event;
      window.__web2codexCompleted = true;
    }
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent, Event: FakeEvent,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  await window.__web2codexSend({
    request_id: "intermediate-commentary", model: "GPT-6", prompt: "Run agents and summarize their findings",
    mcp_required: true, mcp_action_required: true, tool_work_expected: true, attachments: [],
  });

  const streamed = events.filter(event => event.type === "responses-text").map(event => event.delta).join("");
  assert.equal(submitted, true);
  assert.equal(result.text, `${commentary.join("\n\n")}\n\n${finalSummary}`);
  assert.equal(streamed, result.text);
  assert.equal(JSON.stringify(events).includes("PRIVATE_AGENT_TASK"), false);
  assert.equal(JSON.stringify(events).includes("PRIVATE_AGENT_RESULT"), false);
});

function createRetainedResponseHarness({ onSubmit, onPoll, onResult, onReady, transformInput, retainedConnector = false, connectorName = "Example Connector", connectorPrefix = `${connectorName} ` } = {}) {
  let now = 0;
  const events = [];
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false, closest: () => null,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const message = (id, text) => {
    const markdown = makeElement({ innerText: text });
    const root = makeElement({
      markdown,
      getAttribute: name => name === "data-message-id" ? id : null,
      querySelectorAll: selector => selector.includes(".markdown") ? [markdown] : [],
    });
    markdown.parentElement = root;
    return root;
  };
  const window = { __web2codexRequestId: "first", __web2codexConnectorName: connectorName };
  const harness = { window, events, message, messages: [], users: [], groups: [], generating: false, polls: 0, submissions: 0, submittedPrompts: [], instantModels: [] };
  harness.groupedTurn = (key, text, userText = "Respond briefly") => {
    const assistant = message(null, text);
    assistant.getAttribute = name => name === "data-conversation-role" ? "assistant" : null;
    const markdown = assistant.markdown;
    const userBubble = makeElement({ innerText: userText, matches: selector => selector === "[data-user-message-bubble]" });
    const unit = makeElement({
      children: [assistant, markdown],
      getAttribute: name => name === "data-content-search-unit-key" ? `${key}:assistant` : null,
      contains: node => node === assistant || node === markdown,
      querySelectorAll: selector => selector.includes(".markdown") ? [markdown] : [],
    });
    assistant.parentElement = unit;
    markdown.parentElement = unit;
    assistant.closest = selector => selector === "[data-content-search-unit-key]" ? unit : null;
    markdown.closest = selector => selector === "[data-content-search-unit-key]" ? unit : null;
    const group = makeElement({
      matches: selector => selector === "[data-turn-key]",
      getAttribute: name => name === "data-turn-key" ? key : null,
      querySelector: selector => selector === '[data-conversation-role="assistant"]' ? assistant : null,
      querySelectorAll: selector => selector === "[data-user-message-bubble]" ? [userBubble]
        : selector === "[data-content-search-unit-key]" ? [unit]
          : selector.includes(".markdown") ? [markdown] : [],
    });
    unit.parentElement = group;
    userBubble.parentElement = group;
    return group;
  };
  const sendButton = makeElement({ disabled: false, click() {
    harness.submissions++;
    harness.submittedPrompts.push(composer.innerText);
    harness.polls = 0;
    onSubmit?.(harness);
  } });
  const form = makeElement({ querySelectorAll: selector => selector.includes("send-button") ? [sendButton] : [] });
  let selectedConnector = retainedConnector;
  const pill = makeElement({ innerText: connectorName, getAttribute: name => name === "data-keyword" ? connectorName : null });
  const composer = makeElement({ isContentEditable: true, innerText: "", focus() {}, dispatchEvent() {}, closest: () => form,
    querySelectorAll: () => selectedConnector ? [pill] : [],
  });
  const connectorRow = makeElement({ innerText: connectorName, contains: () => false, click() {
    selectedConnector = true;
    composer.innerText = connectorPrefix;
  } });
  harness.composer = composer;
  let appendSelection = false;
  const stopButton = makeElement({ innerText: "Stop generating" });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [composer];
      if (selector === "button") return harness.generating ? [stopButton] : [];
      if (selector.includes('[data-turn-key]:has([data-user-message-bubble])')) return [
        ...harness.users,
        ...harness.groups.filter(group => group.querySelectorAll("[data-user-message-bubble]").length),
      ];
      if (selector.includes('[data-turn-key]:has([data-conversation-role="assistant"]')) return [...harness.messages, ...harness.groups];
      if (selector.startsWith('[data-message-author="user"]')) return harness.users;
      if (selector.startsWith("[data-message-author-role")) return harness.messages;
      if (retainedConnector && selector.includes('[data-list-navigation-item="true"]')) return [connectorRow];
      return [];
    },
    createRange: () => ({ selectNodeContents() { appendSelection = false; }, collapse() { appendSelection = true; } }),
    execCommand(command, _ui, value) {
      if (command === "delete") { composer.innerText = ""; selectedConnector = false; }
      if (command === "insertText") composer.innerText = (appendSelection ? composer.innerText : "") + (transformInput ? transformInput(value) : value);
      return true;
    },
  };
  window.top = window;
  window.__web2codexModelPicker = {
    async select() {},
    async selectInstantForCompaction(model) { harness.instantModels.push(model); },
    async setEffort() {},
  };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    events.push(event);
    if (event.type === "responses-tool-state") {
      harness.polls++;
      if (onPoll) onPoll(harness);
      else window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 5000 };
    }
    if (event.type === "responses-result") {
      window.__web2codexCompleted = true;
      onResult?.(harness, event);
    }
    if (event.type === "responses-bridge-ready") onReady?.(harness, event);
  } };
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, URL,
    location: { origin: "https://chatgpt.com", pathname: "/", href: "https://chatgpt.com/?temporary-chat=true" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    getSelection: () => ({ removeAllRanges() {}, addRange() {} }),
    HTMLTextAreaElement: class {}, HTMLInputElement: class {}, InputEvent: FakeEvent,
    Event: FakeEvent, Uint8Array, atob,
    Date: class extends Date { static now() { return now += 500; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/responses_bridge.js"), "utf8"), context);
  harness.send = (requestId = "first", extra = {}) => window.__web2codexSend({
    request_id: requestId, model: "GPT-6", prompt: "Respond briefly", attachments: [], ...extra,
  });
  harness.results = () => events.filter(event => event.type === "responses-result");
  harness.streamed = () => events.filter(event => event.type === "responses-text").map(event => event.delta).join("");
  return harness;
}

test("compaction uses three acknowledged staging messages and one final message", async () => {
  const acknowledgements = ["W2C_ACK_tx_1", "W2C_ACK_tx_2", "W2C_ACK_tx_3"];
  const h = createRetainedResponseHarness({ onSubmit(h) {
    const index = h.submissions - 1;
    if (index < acknowledgements.length) {
      h.messages.push(h.message(`ack-${index + 1}`, acknowledgements[index]));
    } else {
      h.messages.push(h.message("summary", "Compact continuation summary"));
    }
  } });

  await h.send("first", {
    prompt: "Context part 4/4 and summarize the supplied history",
    stages: acknowledgements.map((acknowledgement, index) => ({
      prompt: `Context part ${index + 1}/4`, acknowledgement,
    })),
  });

  assert.equal(h.submissions, 4);
  assert.deepEqual(h.instantModels, ["GPT-6"]);
  assert.deepEqual(h.submittedPrompts, [
    "Context part 1/4",
    "Context part 2/4",
    "Context part 3/4",
    "Context part 4/4 and summarize the supplied history",
  ]);
  assert.equal(h.events.filter(event => event.type === "responses-prompt-submitted").length, 2);
  assert.equal(h.results()[0].text, "Compact continuation summary");
});

test("historical DOM removal and remount cannot replay older messages or hide a new identical answer", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.messages = [h.message("old-2", "Historical text after hydration"), h.message("new", "Same answer")];
  } });
  h.messages = [h.message("old-1", "Earlier answer"), h.message("old-2", "Same answer")];
  await h.send();
  assert.equal(h.results()[0].text, "Same answer");
  assert.equal(h.streamed(), "Same answer");
  assert.equal(h.submissions, 1);
});

test("grouped ChatGPT turns capture the assistant answer and use the group key as turn identity", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.groups = [h.groupedTurn("turn-key-42", "WEB2CODEX_GROUPED_TURN_OK", "PRIVATE_GROUP_USER_PROMPT")];
  } });

  await h.send();

  assert.equal(h.results()[0].text, "WEB2CODEX_GROUPED_TURN_OK");
  assert.equal(h.streamed(), "WEB2CODEX_GROUPED_TURN_OK");
  assert.equal(h.streamed().includes("PRIVATE_GROUP_USER_PROMPT"), false);
  assert.equal(h.submissions, 1);
});

test("an assistant message hydrated before the submitted user turn is not part of the response", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    const submittedUser = h.message("new-user", "Respond briefly");
    h.messages[1].markdown.innerText = "Unrelated previous answer";
    submittedUser.compareDocumentPosition = message => message === h.messages[1] ? 2 : 4;
    h.users = [submittedUser];
    h.messages.push(h.message("current-answer", "Current answer"));
  } });
  h.messages = [h.message("old-1", "Earlier answer"), h.message("old-2", "Previous answer")];

  await h.send();

  assert.equal(h.results()[0].text, "Current answer");
  assert.equal(h.streamed(), "Current answer");
  assert.equal(h.submissions, 1);
});

test("a reused last message and a newly appended message both belong to the response", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.messages[0].markdown.innerText = "Historical answer hydrated";
    h.messages[1].markdown.innerText = "New commentary";
    h.messages.push(h.message("final", "Final answer"));
  } });
  h.messages = [h.message("old-1", "Earlier answer"), h.message("old-2", "Previous answer")];
  await h.send();
  assert.equal(h.results()[0].text, "New commentary\n\nFinal answer");
  assert.equal(h.streamed(), "New commentary\n\nFinal answer");
});

test("a retained page waits for the previous observer and discards stale tool snapshots", async () => {
  let rebound;
  const h = createRetainedResponseHarness({
    onSubmit(h) { h.messages.push(h.message(`answer-${h.submissions}`, `Answer ${h.submissions}`)); },
    onPoll(h) {
      if (h.submissions === 1 || h.polls >= 3) {
        h.window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: 5000 };
      }
    },
    onResult(h, event) {
      if (event.request_id === "first") {
        h.window.__web2codexToolState = { active: false, connector_requests: 5, tool_invocations: 5 };
        // Native may rebind before the old observer resumes from its sleep.
        rebound = h.window.__web2codexBindRequest("second");
      }
    },
  });
  await h.send();
  assert.equal(await rebound, true);
  assert.equal(h.window.__web2codexToolState, null);
  h.window.__web2codexToolState = { active: false };
  await h.send("second");
  assert.equal(h.results().length, 2);
  assert.equal(h.results()[1].text, "Answer 2");
  assert.ok(h.polls >= 3);
  assert.equal(h.submissions, 2);
});

test("a repeated prompt replaces restored drafts and is submitted only once per request", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.messages.push(h.message(`answer-${h.submissions}`, "OK"));
  } });
  h.composer.innerText = "Previously restored draft";
  await h.send("first", { prompt: "Repeat this" });
  await h.send("first", { prompt: "Repeat this" });
  assert.equal(await h.window.__web2codexBindRequest("second"), true);
  h.composer.innerText = "Repeat this";
  await h.send("second", { prompt: "Repeat this" });
  assert.deepEqual(h.submittedPrompts, ["Repeat this", "Repeat this"]);
  assert.deepEqual(h.results().map(result => result.text), ["OK", "OK"]);
});

test("a corrupted prompt with the correct tail is rejected before submission", async () => {
  const h = createRetainedResponseHarness({ transformInput: value => `stale draft ${value}` });
  await h.send("first", { prompt: "x".repeat(140) });
  assert.equal(h.submissions, 0);
  assert.match(h.results()[0].error, /did not preserve the complete prompt text/);
  assert.equal(h.composer.innerText, "");
});

test("contenteditable structural line breaks do not reject intact prompt text", async () => {
  const h = createRetainedResponseHarness({ transformInput: value => value.replace(/\n/g, "\n\n"), onSubmit(h) {
    h.messages.push(h.message("reply", "OK"));
  } });
  const prompt = "First line\nSecond line\n\nCode:\nprint('ok')";
  await h.send("first", { prompt });
  assert.equal(h.submissions, 1);
  assert.equal(h.results()[0].text, "OK");
});

test("a rejected native reasoning effort stops the request before prompt submission", async () => {
  const h = createRetainedResponseHarness();
  h.window.__web2codexModelPicker.setEffort = async effort => {
    assert.equal(effort, "high");
    throw new Error("ChatGPT did not retain high reasoning effort");
  };
  await h.send("first", { reasoning_effort: "high" });
  assert.equal(h.submissions, 0);
  assert.equal(h.results().length, 1);
  assert.match(h.results()[0].error, /did not retain high reasoning effort/);
  assert.equal(h.events.some(event => event.type === "responses-prompt-submitted"), false);
  assert.equal(h.composer.innerText, "");
});

test("a retained connector with an old draft tolerates ChatGPT's leading mention marker", async () => {
  const connectorName = "Example Connector";
  const h = createRetainedResponseHarness({ retainedConnector: true, connectorName, connectorPrefix: `\n@${connectorName}\n`, onSubmit(h) {
    h.messages.push(h.message("reply", "OK"));
  } });
  h.composer.innerText = `${connectorName} old draft`;
  await h.send("first", { connector_required: true, prompt: "New prompt" });
  assert.deepEqual(h.submittedPrompts, [`\n@${connectorName}\nNew prompt`]);
  assert.equal(h.results()[0].text, "OK");
});

test("a reused node with a prefix-related short reply keeps the entire reply", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.messages[0].markdown.innerText = "Окей";
  } });
  h.messages = [h.message("old", "Ок")];
  await h.send();
  assert.equal(h.results()[0].text, "Окей");
  assert.equal(h.streamed(), "Окей");
});

test("an identical reused reply needs evidence that it follows the new user turn", async () => {
  const h = createRetainedResponseHarness({ onSubmit(h) {
    h.users.push(h.message("new-user", "Respond briefly"));
    h.users.at(-1).compareDocumentPosition = () => h.polls >= 3 ? 4 : 2;
  } });
  h.users = [h.message("old-user", "Respond briefly")];
  h.messages = [h.message("old-answer", "OK")];
  await h.send();
  assert.equal(h.results()[0].text, "OK");
  assert.equal(h.streamed(), "OK");
  assert.ok(h.polls >= 3);
});

test("overlapping page bindings publish readiness only for the current request", async () => {
  const h = createRetainedResponseHarness();
  const first = h.window.__web2codexBindRequest("second");
  const next = h.window.__web2codexBindRequest("third");
  assert.equal(await first, false);
  assert.equal(await next, true);
  assert.deepEqual(h.events.filter(event => event.type === "responses-bridge-ready").map(event => event.request_id), ["third"]);
});

test("retained binding waits for a delayed native completion acknowledgement", async () => {
  let rebound;
  const h = createRetainedResponseHarness({
    onSubmit(h) { h.messages.push(h.message(`answer-${h.submissions}`, `Answer ${h.submissions}`)); },
    onResult(h, event) {
      if (event.request_id !== "first") return;
      h.window.__web2codexCompleted = false;
      rebound = h.window.__web2codexBindRequest("second");
      queueMicrotask(() => { h.window.__web2codexCompleted = true; });
    },
  });
  await h.send();
  assert.equal(await rebound, true);
  await h.send("second");
  assert.deepEqual(h.results().map(result => result.text), ["Answer 1", "Answer 2"]);
  assert.equal(h.submissions, 2);
});

test("a stuck generation ends after bounded inactivity without submitting again", async () => {
  const h = createRetainedResponseHarness({
    onSubmit(h) { h.generating = true; },
    onPoll(h) {
      h.window.__web2codexToolState = { active: true, revision: 0, pending_tools: 0, completed_tools: 0, quiet_ms: h.polls * 1000 };
      if (h.polls > 2000) throw new Error("test exceeded the expected stall budget");
    },
  });
  await h.send();
  assert.match(h.results()[0].error, /stopped making observable progress while generating/);
  assert.equal(h.submissions, 1);
  assert.equal(h.streamed(), "");
});

test("independent commentary and final answer retain coincidental text overlap", async () => {
  const h = createRetainedResponseHarness({
    onSubmit(h) { h.messages = [h.message("reply", "The first agent is ready")]; },
    onPoll(h) {
      const completed = h.polls > 1;
      h.window.__web2codexToolState = { active: true, revision: completed ? 2 : 1, pending_tools: completed ? 0 : 1, completed_tools: completed ? 1 : 0, quiet_ms: 5000 };
      if (completed) h.messages[0].markdown.innerText = "ready to continue with the final result";
    },
  });
  await h.send("first", { tool_work_expected: true });
  const expected = "The first agent is ready\n\nready to continue with the final result";
  assert.equal(h.results()[0].text, expected);
  assert.equal(h.streamed(), expected);
});

test("a new final message identical to preserved commentary completes successfully", async () => {
  const h = createRetainedResponseHarness({
    onSubmit(h) { h.messages = [h.message("commentary", "Ready")]; },
    onPoll(h) {
      const completed = h.polls > 1;
      h.window.__web2codexToolState = {
        active: true,
        revision: completed ? 2 : 1,
        pending_tools: completed ? 0 : 1,
        completed_tools: completed ? 1 : 0,
        quiet_ms: 5000,
      };
      if (completed) h.messages = [h.message("final", "Ready")];
    },
  });

  await h.send("first", { tool_work_expected: true });

  assert.equal(h.results()[0].text, "Ready");
  assert.equal(h.results()[0].error, undefined);
  assert.equal(h.streamed(), "Ready");
  assert.equal(h.submissions, 1);
});
