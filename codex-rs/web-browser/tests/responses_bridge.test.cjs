const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

test("a request uses the re-rendered composer, attaches image and audio files, then submits", async () => {
  let now = 0;
  let submitted = false;
  let attached = false;
  let uploadedNames = [];
  const nativeEvents = [];
  let result;
  const makeElement = extra => ({
    isConnected: true, parentElement: null, matches: () => false,
    getAttribute: () => null, querySelector: () => null, querySelectorAll: () => [],
    ...extra,
  });
  const pill = makeElement({ getAttribute: name => name === "data-keyword" ? "Web2Codex" : null });
  const input = makeElement({
    getAttribute: name => name === "accept" ? "image/*,audio/*" : null,
    dispatchEvent(event) {
      if (event.type === "change") { uploadedNames = this.files.map(file => file.name); attached = true; }
    },
  });
  const submit = makeElement({ disabled: false, click() { submitted = true; } });
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
  const row = makeElement({
    innerText: "Web2Codex",
    click() { attachedConnector = true; currentComposer.innerText = ""; },
    contains: () => false,
  });
  const markdown = makeElement({ innerText: "Done." });
  const assistant = makeElement({ querySelectorAll: () => [markdown] });
  const document = {
    querySelectorAll(selector) {
      if (selector.startsWith("#prompt-textarea")) return [currentComposer];
      if (selector.startsWith(".__menu-item")) return [row];
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
  const window = { __web2codexRequestId: "request-1" };
  window.top = window;
  window.__web2codexModelPicker = {
    async select() { oldComposer.isConnected = false; currentComposer = composer(); },
    async setEffort() {},
  };
  window.ipc = { postMessage(message) {
    const event = JSON.parse(message);
    nativeEvents.push(event);
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
    attachments: [
      { name: "web2codex-image-1.png", mime: "image/png", data_url: "data:image/png;base64,aGVsbG8=" },
      { name: "web2codex-audio-2.wav", mime: "audio/wav", data_url: "data:audio/wav;base64,aGVsbG8=" },
    ],
  });
  assert.equal(oldComposer.innerText, "");
  assert.deepEqual(uploadedNames, ["web2codex-image-1.png", "web2codex-audio-2.wav"]);
  assert.equal(submitted, true);
  assert.equal(result.text, "Done.");
  assert.equal(result.error, undefined);
  const textEventIndex = nativeEvents.findIndex(event => event.type === "responses-text");
  const resultEventIndex = nativeEvents.findIndex(event => event.type === "responses-result");
  assert.ok(textEventIndex >= 0 && textEventIndex < resultEventIndex);
  assert.equal(nativeEvents[textEventIndex].delta, "Done.");
});
