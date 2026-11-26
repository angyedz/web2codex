const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

test("Instant maps to the localized low option in ChatGPT's reasoning control", async () => {
  const levels = ["Лёгкое", "Средний", "Высокий"];
  let level = 1;
  let min = 0;
  let nativeFailure = false;
  let menuOpen = false;
  let background = false;
  let keyboardIgnored = false;
  let tickIgnored = false;
  let pointerIgnored = false;
  let nativeUncommitted = false;
  let now = 0;
  let genericClicks = 0;
  let tickClicks = 0;
  let backgroundFocusRequests = 0;
  const element = (rect, extra = {}) => ({
    isConnected: true,
    parentElement: null,
    matches: () => false,
    getBoundingClientRect: () => rect,
    querySelector: () => null,
    querySelectorAll: () => [],
    getAttribute: () => null,
    contains: () => false,
    ...extra,
  });
  let slider;
  const sliderRoot = element({ x: 0, y: 40, width: 100, height: 20 }, {
    getAttribute: name => name === "data-orientation" ? "horizontal" : null,
    querySelector: () => slider,
    dispatchEvent(event) {
      if (event.type === "pointerdown" && !pointerIgnored) {
        level = Math.max(0, Math.min(2, Math.round((event.clientX - 14) / 72 * 2)));
      }
      return true;
    },
  });
  const sliderWrapper = element({ x: 36, y: 36, width: 28, height: 28 }, {
    parentElement: sliderRoot,
    querySelector: () => slider,
  });
  slider = element({ x: 36, y: 36, width: 28, height: 28 }, {
    parentElement: sliderWrapper,
    focus() { if (background) backgroundFocusRequests++; },
    getAttribute(name) {
      return { "aria-valuemin": String(min), "aria-valuemax": String(min + 2), "aria-valuenow": String(min + level) }[name] ?? null;
    },
    dispatchEvent(event) {
      if (event.type === "keydown" && !keyboardIgnored) level += event.key === "ArrowLeft" ? -1 : 1;
      return true;
    },
  });
  const row = element({ x: 0, y: 50, width: 100, height: 30 }, {
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]' && keyboardIgnored ? levels.map((label, index) => element({ x: index * 30, y: 50, width: 20, height: 20 }, {
      getAttribute: name => name === "aria-label" ? label : name === "data-locked" ? "false" : null,
      dispatchEvent: () => true,
      click() { tickClicks++; if (!tickIgnored) level = index; },
    })) : [],
  });
  Object.defineProperty(row, "innerText", { get: () => levels[level] });
  const control = element({ x: 0, y: 0, width: 100, height: 30 }, {
    click() { menuOpen = !menuOpen; },
    getAttribute: name => name === "aria-expanded" ? String(menuOpen) : null,
    matches: selector => selector === '[data-composer-navigation-target="reasoning"]',
    closest: () => null,
  });
  const option = element({ x: 0, y: 100, width: 100, height: 30 }, {
    innerText: "GPT-6",
    getAttribute: name => name === "aria-checked" ? "true" : null,
  });
  const generic = element({ x: 0, y: 0, width: 100, height: 30 }, {
    innerText: "Medium", closest: () => null,
    click() { genericClicks++; },
  });
  const document = {
    get visibilityState() { return background ? "hidden" : "visible"; },
    hasFocus: () => !background,
    querySelectorAll(selector) {
      if (selector.includes("[data-model-id]")) return menuOpen ? [option] : [];
      if (selector.includes("data-composer-navigation-target")) return [generic, control];
      if (selector.includes("data-effort-only")) return menuOpen ? [row] : [];
      return [];
    },
    dispatchEvent(event) { if (event.key === "Escape") menuOpen = false; },
  };
  let nativeSliderIndex = null;
  const window = { ipc: { postMessage(message) {
    const event = JSON.parse(message);
    if (event.type === "set-power-slider") {
      assert.equal(event.allow_focus, !background, "native input must preserve the background focus restriction");
      if (background) assert.ok(tickClicks > 1, "background native fallback must follow DOM attempts");
      nativeSliderIndex = event.index;
      if (nativeFailure) window.__web2codexPowerSliderResults = {
        [event.request_id]: { ok: false, error: "ChatGPT did not expose one visible three-position power slider" },
      };
      else if (nativeUncommitted) window.__web2codexPowerSliderResults = { [event.request_id]: { ok: true } };
      else level = event.index;
    }
  } } };
  window.top = window;
  class FakeEvent {
    constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
  }
  const context = vm.createContext({
    window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent,
    Date: class extends Date { static now() { return now += 100; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; }, clearTimeout,
  });
  const script = fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8");
  vm.runInContext(script, context);
  await window.__web2codexCapabilityProbeComplete;
  await window.__web2codexModelPicker.setEffort("Instant");
  assert.equal(nativeSliderIndex, 0);
  assert.equal(levels[level], "Лёгкое");
  await assert.rejects(window.__web2codexModelPicker.setEffort("xhigh"), /does not make xhigh reasoning effort available/);
  assert.equal(levels[level], "Лёгкое");
  nativeFailure = true;
  await window.__web2codexModelPicker.setEffort("high");
  assert.equal(nativeSliderIndex, 2);
  assert.equal(level, 2);
  min = 1;
  nativeSliderIndex = null;
  await window.__web2codexModelPicker.setEffort("medium");
  assert.equal(nativeSliderIndex, null);
  assert.equal(level, 1);
  min = 0;
  background = true;
  keyboardIgnored = true;
  tickIgnored = true;
  await window.__web2codexModelPicker.setEffort("low");
  assert.equal(level, 0);
  assert.equal(tickClicks, 1, "ineffective tick clicks should fall through to the hidden DOM slider");
  assert.ok(backgroundFocusRequests > 0, "DOM focus should let the hidden slider handle keyboard input");
  assert.equal(nativeSliderIndex, null);
  tickIgnored = false;
  await window.__web2codexModelPicker.setEffort("high");
  assert.equal(level, 2);
  assert.equal(tickClicks, 2);
  assert.equal(genericClicks, 0, "explicit composer trigger outranks a generic effort button");
  assert.equal(menuOpen, false);
  tickIgnored = true;
  pointerIgnored = true;
  nativeFailure = false;
  await window.__web2codexModelPicker.setEffort("low");
  assert.equal(nativeSliderIndex, 0, "native fallback runs after ineffective DOM input");
  assert.equal(level, 0);
  nativeUncommitted = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not apply high reasoning effort/);
  assert.equal(level, 0, "successful native acknowledgement alone must not confirm effort");
  assert.equal(menuOpen, false);
  nativeUncommitted = false;
  nativeFailure = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not apply high reasoning effort: ChatGPT did not expose/);
  assert.equal(level, 0);
  assert.equal(menuOpen, false);
});

test("the neutral picker opens its model list and changes an ARIA slider", async () => {
  let modelList = false;
  let menuOpen = false;
  let selected = 1;
  let highLocked = true;
  let sixPositionControl = false;
  let fourPositionControl = false;
  let sliderDisabled = false;
  const element = (rect, extra = {}) => ({
    isConnected: true, parentElement: null, matches: () => false, contains: () => false,
    getBoundingClientRect: () => rect, querySelector: () => null,
    querySelectorAll: () => [], getAttribute: () => null, dispatchEvent: () => true,
    ...extra,
  });
  const sliderControl = element({}, {
    focus() {},
    dispatchEvent(event) {
      if (event.type === "keydown") selected += event.key === "ArrowRight" ? 1 : -1;
      return true;
    },
  });
  const slider = element({}, {
    closest: selector => selector === '[role="menuitem"]' ? sliderControl : null,
    getAttribute(name) {
      return { "aria-valuemin": "0", "aria-valuemax": sixPositionControl ? "5" : fourPositionControl ? "3" : "4", "aria-valuenow": String(selected), "aria-disabled": String(sliderDisabled) }[name] ?? null;
    },
  });
  const control = element({ x: 0, y: 0, width: 100, height: 30 }, {
    innerText: "Medium", click() { menuOpen = !menuOpen; modelList = false; }, closest: () => null,
    getAttribute: name => name === "aria-expanded" ? String(menuOpen) : null,
    matches: selector => selector === '[data-tone="neutral"]',
  });
  const sliderContainer = element({ x: 0, y: 50, width: 100, height: 30 }, {
    matches: selector => selector.includes("data-model-picker-power-slider"),
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]'
      ? Array.from({ length: sixPositionControl ? 6 : fourPositionControl ? 4 : 5 }, (_, index) => ({
        getAttribute: name => name === "data-locked" && index === 2 && highLocked ? "true" :
          name === "data-locked" ? "false" :
          name === "aria-label" && (sixPositionControl || sliderDisabled) ? ["Instant", "Medium", "High", "Extra High", "Pro", "unrecognized"][index] : null,
      })) : [],
  });
  const toggle = element({ x: 0, y: 30, width: 100, height: 20 }, {
    innerText: "Medium",
    click() { modelList = true; },
  });
  const option = element({ x: 0, y: 100, width: 100, height: 30 }, {
    innerText: "GPT-6", getAttribute: name => name === "aria-checked" ? "true" : null,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("[data-model-id]")) return menuOpen && modelList ? [option] : [];
      if (selector.includes("data-composer-navigation-target")) return [control];
      if (selector.includes("data-effort-only")) return menuOpen && !modelList ? [toggle, sliderContainer] : [];
      if (selector.includes("data-model-picker-view-toggle")) return menuOpen && !modelList ? [toggle] : [];
      return [];
    },
    dispatchEvent(event) { if (event.key === "Escape") { menuOpen = false; modelList = false; } },
  };
  const messages = [];
  const window = { ipc: { postMessage(message) { messages.push(JSON.parse(message)); } } };
  window.top = window;
  class FakeEvent {
    constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
  }
  const context = vm.createContext({
    window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent, setTimeout, clearTimeout,
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);
  await window.__web2codexCapabilityProbeComplete;
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "xhigh", "max"]);
  await window.__web2codexModelPicker.select("GPT-6");
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /does not make high reasoning effort available/);
  assert.equal(selected, 1);
  highLocked = false;
  await window.__web2codexModelPicker.setEffort("high");
  assert.equal(selected, 2);
  await window.__web2codexModelPicker.setEffort("xhigh");
  assert.equal(selected, 3);
  await window.__web2codexModelPicker.setEffort("max");
  assert.equal(selected, 4);
  sixPositionControl = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /does not make high reasoning effort available/);
  assert.equal(selected, 4);
  sixPositionControl = false;
  fourPositionControl = true;
  selected = 1;
  await window.__web2codexModelPicker.setEffort("xhigh");
  assert.equal(selected, 3);
  await assert.rejects(window.__web2codexModelPicker.setEffort("max"), /does not make max reasoning effort available/);
  sliderDisabled = true;
  selected = 1;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /does not make high reasoning effort available/);
  assert.equal(selected, 1, "disabled slider must not receive keyboard input");
  window.__web2codexDeepProbe = false;
  await window.__web2codexModelPicker.select("GPT-6");
  assert.deepEqual(messages.at(-1).reasoning_by_model["GPT-6"], [], "disabled slider must not advertise available levels");
});

test("a localized portal slider reuses its open surface and confirms a fourth level after reopening", async () => {
  let open = true;
  let level = 1;
  let clicks = 0;
  const labels = ["Мгновенный", "Средний", "Высокий", "Очень высокий"];
  const element = (rect, extra = {}) => ({
    isConnected: true, parentElement: null, matches: () => false, contains: () => false,
    getBoundingClientRect: () => rect, querySelector: () => null,
    querySelectorAll: () => [], getAttribute: () => null, closest: () => null,
    dispatchEvent: () => true, ...extra,
  });
  const control = element({ x: 20, y: 700, width: 100, height: 32 }, {
    innerText: "Рассуждение",
    getAttribute(name) {
      return { "aria-expanded": String(open), "aria-controls": "effort-menu", "data-selected-reasoning-effort": labels[level] }[name] ?? null;
    },
    click() { clicks++; open = !open; },
  });
  const menuItem = element({}, {
    focus() {},
    dispatchEvent(event) {
      if (event.type === "keydown") level += event.key === "ArrowRight" ? 1 : -1;
      return true;
    },
  });
  const slider = element({ width: 0, height: 0 }, {
    closest: selector => selector === '[role="menuitem"]' ? menuItem : null,
    getAttribute(name) {
      return { "aria-hidden": "true", "aria-valuemin": "0", "aria-valuemax": "3", "aria-valuenow": String(level), "aria-valuetext": `${labels[level]}, ${level + 1} из 4` }[name] ?? null;
    },
  });
  const row = element({ x: 600, y: 20, width: 180, height: 50 }, {
    matches: selector => selector.includes("data-model-picker-power-slider"),
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]' ? labels.map(label => ({
      getAttribute: name => name === "aria-label" ? `${label}, описание` : null,
    })) : [],
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("data-composer-navigation-target")) return [control];
      // Simulate an outgoing portal which remains visible even when closed.
      if (selector.includes("data-model-picker-power-slider")) return [row];
      return [];
    },
    dispatchEvent(event) { if (event.key === "Escape") open = false; },
  };
  const window = { __web2codexDeepProbe: false, ipc: { postMessage() { assert.fail("four-level slider must not call three-level UIA"); } } };
  window.top = window;
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent,
    setTimeout: callback => { queueMicrotask(callback); return 1; }, clearTimeout,
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);
  await window.__web2codexModelPicker.setEffort("xhigh");
  assert.equal(level, 3);
  assert.equal(clicks, 1, "only the final verification needs to reopen the menu");
  assert.equal(open, false);
});

test("a Medium composer dropdown discovers and applies direct effort choices", async () => {
  let now = 0;
  let view = "closed";
  let currentEffort = "Medium";
  let pointerOnly = false;
  let clicks = 0;
  let pointerOpens = 0;
  let ghostClicks = 0;
  let transientSelection = false;
  let reportedEffort = null;
  let ignoreSelection = false;
  let ignoreEscape = false;
  let ignoreDismiss = false;
  let hydrate = false;
  let replacement = null;
  let detachedReads = 0;
  let modelClicks = 0;
  const messages = [];
  const element = (rect, extra = {}) => ({
    isConnected: true, parentElement: null, matches: () => false, contains: () => false,
    getBoundingClientRect: () => rect, querySelector: () => null,
    querySelectorAll: () => [], getAttribute: () => null, dispatchEvent: () => true,
    ...extra,
  });
  const control = element({ x: 0, y: 0, width: 130, height: 32 }, {
    matches: selector => selector === '[data-tone="neutral"]',
    closest: () => null,
    getAttribute(name) {
      if (name === "aria-expanded") return String(view !== "closed");
      if (name === "aria-controls") return "efforts";
      return null;
    },
    click() {
      clicks++;
      if (hydrate) {
        hydrate = false;
        control.isConnected = false;
        replacement = { ...control, isConnected: true };
        Object.defineProperty(replacement, "innerText", { get: () => reportedEffort ?? currentEffort });
        return;
      }
      if (!pointerOnly && !ignoreDismiss) {
        if (view === "efforts" && transientSelection) currentEffort = "Medium";
        view = view === "closed" ? "efforts" : "closed";
      }
    },
    dispatchEvent(event) {
      if (event.type === "pointerdown" && event.isPrimary && pointerOnly) {
        pointerOpens++;
        view = "efforts";
      }
      return true;
    },
  });
  Object.defineProperty(control, "innerText", { get: () => reportedEffort ?? currentEffort });
  const modelControl = element({ x: 0, y: 0, width: 130, height: 32 }, {
    innerText: "GPT-6",
    closest: () => null,
    getAttribute: name => name === "aria-expanded" ? String(view === "models") : null,
    click() { modelClicks++; view = view === "models" ? "closed" : "models"; },
  });
  const ownedMenu = element({}, { getAttribute: name => name === "id" ? "efforts" : null });
  const ghostMenu = element({}, { getAttribute: name => name === "id" ? "other-menu" : null });
  const ghost = element({ x: 0, y: 60, width: 150, height: 28 }, {
    innerText: "High", parentElement: ghostMenu, click() { ghostClicks++; },
  });
  const efforts = ["Instant", "Medium", "High", "Extra   High"].map((label, index) => element(
    { x: 0, y: 60 + index * 32, width: 150, height: 28 }, {
      innerText: label,
      parentElement: ownedMenu,
      getAttribute(name) {
        if (name === "aria-checked") return String(currentEffort === label);
        return null;
      },
      click() {
        if (ignoreSelection) return;
        currentEffort = label;
        if (!transientSelection) view = "closed";
      },
    },
  ));
  const modelToggle = element({ x: 0, y: 42, width: 140, height: 20 }, {
    click() { view = "models"; },
  });
  const model = element({ x: 0, y: 100, width: 150, height: 30 }, {
    innerText: "GPT-6",
    getAttribute(name) {
      if (name === "data-model-id") return "gpt-6";
      if (name === "aria-checked") return "true";
      return null;
    },
    click() { view = "models"; },
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("data-testid") || selector.includes('aria-label*="model"')) return [modelControl];
      if (selector.includes("data-tone") || selector.includes("data-composer-navigation-target")) {
        if (replacement) return detachedReads++ < 1 ? [] : [replacement];
        return [control];
      }
      if (selector.includes("data-model-picker-view-toggle")) return view === "efforts" ? [modelToggle] : [];
      if (selector.includes("[data-model-id]")) return view === "models" ? [model] : [];
      if (selector.includes('[role="menuitem"]') || selector.includes('[role="option"]') || selector.includes("button")) {
        if (view === "efforts") return [control, ghost, ...efforts];
        if (view === "models") return [model];
      }
      return [];
    },
    dispatchEvent(event) {
      if (event.key === "Escape" && !ignoreEscape) {
        if (transientSelection && view === "efforts") currentEffort = "Medium";
        view = "closed";
      }
    },
  };
  const window = { ipc: { postMessage(message) { messages.push(JSON.parse(message)); } } };
  window.top = window;
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent,
    Date: class extends Date { static now() { return now += 1000; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; }, clearTimeout,
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);
  await window.__web2codexCapabilityProbeComplete;

  assert.equal(window.__web2codexCapabilityProbeError, undefined);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high", "xhigh"]);
  await window.__web2codexModelPicker.setEffort("High").catch(error => {
    throw new Error(`${error.message}; current=${currentEffort}; view=${view}`);
  });
  assert.equal(currentEffort, "High");
  assert.equal(ghostClicks, 0);
  pointerOnly = true;
  await window.__web2codexModelPicker.setEffort("xhigh");
  assert.equal(pointerOpens, 2, "selection and verification both need the pointer-open fallback");
  assert.equal(currentEffort, "Extra   High");
  view = "efforts";
  const clicksBefore = clicks;
  await window.__web2codexModelPicker.setEffort("medium");
  assert.equal(clicks, clicksBefore + 1, "only verification needs to reopen the existing menu");
  assert.equal(pointerOpens, 3);
  assert.equal(currentEffort, "Medium");
  pointerOnly = false;
  transientSelection = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not retain high reasoning effort/);
  assert.equal(currentEffort, "Medium");
  assert.equal(view, "closed");
  ignoreEscape = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not retain high reasoning effort/);
  assert.equal(currentEffort, "Medium", "trigger dismissal must discard a transient selection too");
  assert.equal(view, "closed");
  view = "efforts";
  ignoreDismiss = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not close its picker menu/);
  assert.equal(view, "efforts", "an undismissable surface must never confirm persistence");
  ignoreDismiss = false;
  ignoreEscape = false;
  view = "closed";
  currentEffort = "Medium";
  transientSelection = false;
  reportedEffort = "High";
  ignoreSelection = true;
  await assert.rejects(window.__web2codexModelPicker.setEffort("high"), /did not retain high reasoning effort/);
  assert.equal(currentEffort, "Medium", "a contradictory trigger label must not override the selected option");
  assert.equal(view, "closed");
  reportedEffort = null;
  ignoreSelection = false;
  await window.__web2codexModelPicker.setEffort("high");
  assert.equal(currentEffort, "High");
  assert.equal(view, "closed");
  const previousModelClicks = modelClicks;
  hydrate = true;
  await window.__web2codexModelPicker.setEffort("medium");
  assert.equal(currentEffort, "Medium");
  assert.equal(modelClicks, previousModelClicks, "a temporarily detached composer trigger must never open the header model menu");
  assert.equal(view, "closed");
});

async function runDeepProbe({
  initialModel = "GPT-6",
  failSelection = () => false,
  unreliableEffortFor = null,
  threePositionPower = false,
  threePositionMenu = false,
  threePositionMarker = "data-model-picker-power-slider",
  modelNames = ["GPT-6", "GPT-5.6 Sol", "o4-mini", "Auto"],
  subtitles = {},
  modelIds = true,
} = {}) {
  let view = "closed";
  let current = initialModel;
  const messages = [];
  const element = (rect, extra = {}) => ({
    isConnected: true, parentElement: null, matches: () => false, contains: () => false,
    getBoundingClientRect: () => rect, querySelector: () => null, querySelectorAll: () => [],
    getAttribute: () => null, dispatchEvent: () => true, ...extra,
  });
  const control = element({ x: 0, y: 0, width: 100, height: 30 }, {
    innerText: "Medium", closest: () => null,
    getAttribute: name => name === "aria-expanded" ? String(view !== "closed") : null,
    click() { view = view === "closed" ? "effort" : "closed"; },
  });
  const slider = element({}, {
    getAttribute: name => ({
      "aria-valuemin": threePositionPower ? "1" : "0",
      "aria-valuemax": threePositionPower ? "3" : current === unreliableEffortFor ? "5" : "4",
      "aria-valuenow": threePositionPower ? "2" : "1",
      "aria-valuetext": threePositionPower ? "Medium" : null,
    })[name] ?? null,
  });
  const row = element({ x: 0, y: 50, width: 100, height: 30 }, {
    innerText: threePositionPower ? "Medium, 2 of 3" : "",
    matches: selector => selector.includes(threePositionMarker),
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]'
      ? (threePositionPower ? [] : Array.from({ length: 5 }, (_, index) => ({
        getAttribute: name => name === "data-locked" && current === "GPT-5.6 Sol" && index >= 3 ? "true" : "false",
      }))) : [],
  });
  const toggle = element({ x: 0, y: 30, width: 100, height: 20 }, { click() { view = "models"; } });
  const options = modelNames.map(name => element(
    { x: 0, y: 100, width: 100, height: 30 }, {
      innerText: subtitles[name] ? `${name} ${subtitles[name]}` : name,
      querySelector: selector => selector === '[data-menu-row-content] span:first-child' && subtitles[name]
        ? { innerText: name } : null,
      getAttribute: attr => attr === "aria-checked" ? String(current === name)
        : attr === "data-model-id" && modelIds && name !== "Auto" ? name.toLowerCase().replace(/[^a-z0-9]/g, "") : null,
      click() {
        if (!failSelection(name, current)) current = name;
        view = "effort";
      },
    },
  ));
  const document = {
    querySelectorAll(selector) {
      if (selector.includes('[role="menu"]') && view === "effort" && threePositionMenu) return [row];
      if (selector.includes("[data-model-id]")) return view === "models" ? options : [];
      if (selector.includes("data-composer-navigation-target")) return [control];
      if (selector.includes("data-effort-only")) return view === "effort" && !threePositionMenu ? [row] : [];
      if (selector.includes("data-model-picker-view-toggle")) return view === "effort" ? [toggle] : [];
      return [];
    },
    dispatchEvent(event) { if (event.key === "Escape") view = "closed"; },
  };
  const window = { __web2codexDeepProbe: true, ipc: { postMessage(message) { messages.push(JSON.parse(message)); } } };
  window.top = window;
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({ window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent,
    setTimeout: (callback, delay) => setTimeout(callback, Math.min(delay, 5)), clearTimeout });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);
  await window.__web2codexCapabilityProbeComplete;
  return { current, messages, select: async label => {
    await window.__web2codexModelPicker.select(label);
    return current;
  }, selectInstant: async label => {
    await window.__web2codexModelPicker.selectInstantForCompaction(label);
    return current;
  } };
}

test("compaction selects only an available Instant model from the same family", async () => {
  const { current, selectInstant } = await runDeepProbe({
    initialModel: "GPT-6.2 Thinking",
    modelNames: ["GPT-6.2 Thinking", "GPT-6.2 Instant", "GPT-6 Instant", "GPT-5.6 Sol", "GPT-5.6 Instant"],
  });

  assert.equal(current, "GPT-6.2 Thinking");
  assert.equal(await selectInstant("GPT-6.2 Thinking"), "GPT-6.2 Instant");
  assert.equal(await selectInstant("GPT-5.6 Sol"), "GPT-5.6 Instant");
});

test("compaction stops when the matching Instant model is unavailable", async () => {
  const { current, selectInstant } = await runDeepProbe({
    initialModel: "GPT-6.2 Thinking",
    modelNames: ["GPT-6.2 Thinking", "GPT-6 Instant"],
  });

  await assert.rejects(selectInstant("GPT-6.2 Thinking"), /does not expose one available Instant model/);
  assert.equal(current, "GPT-6.2 Thinking");
});

test("the initial probe publishes a complete snapshot only after scanning every model and restoring the original", async () => {
  const { current, messages } = await runDeepProbe();

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].picker_options, ["GPT-6", "GPT-5.6 Sol", "o4-mini", "Auto"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high", "xhigh", "max"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-5.6 Sol"], ["low", "medium", "high"]);
});

test("version-only model rows and lifecycle subtitles preserve exact stable picker labels", async () => {
  const subtitle = "Будет недоступна с 23 декабря";
  const { current, messages, select } = await runDeepProbe({
    initialModel: "6.2",
    modelNames: ["6.2", "6", "GPT-5.6 Sol"],
    subtitles: { "GPT-5.6 Sol": subtitle },
    modelIds: false,
  });

  assert.equal(current, "6.2");
  assert.deepEqual(messages[0].picker_options, ["6.2", "6", "GPT-5.6 Sol"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-5.6 Sol"], ["low", "medium", "high"]);
  assert.equal(await select(`GPT-5.6 Sol ${subtitle}`), "GPT-5.6 Sol");
  assert.equal(await select("6"), "6");
  assert.equal(await select("GPT-5.6 Sol"), "GPT-5.6 Sol");
});

test("a deep probe with no uniquely selected original model publishes no completed snapshot", async () => {
  const { current, messages } = await runDeepProbe({ initialModel: null });

  assert.equal(current, null);
  assert.equal(messages.length, 0);
});

test("a model that cannot be selected reliably does not block other verified model capabilities", async () => {
  const { current, messages } = await runDeepProbe({
    failSelection: name => name === "GPT-5.6 Sol",
  });

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].picker_options, ["GPT-6", "GPT-5.6 Sol", "o4-mini", "Auto"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high", "xhigh", "max"]);
  assert.equal(messages[0].reasoning_by_model["GPT-5.6 Sol"], undefined);
});

test("an unverified effort range for one model does not erase other detected effort levels", async () => {
  const { current, messages } = await runDeepProbe({ unreliableEffortFor: "GPT-5.6 Sol" });

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].picker_options, ["GPT-6", "GPT-5.6 Sol", "o4-mini", "Auto"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high", "xhigh", "max"]);
  assert.equal(messages[0].reasoning_by_model["GPT-5.6 Sol"], undefined);
});

test("an unlabeled three-position power slider uses its verified current effort label", async () => {
  const { current, messages } = await runDeepProbe({
    threePositionPower: true,
    threePositionMarker: "data-model-reasoning-effort-slider",
  });

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high"]);
});

test("a three-position effort slider uses its current label even without a data marker", async () => {
  const { current, messages } = await runDeepProbe({
    threePositionPower: true,
    threePositionMarker: "missing-marker",
  });

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high"]);
});

test("a three-position slider is detected from its enclosing power menu label", async () => {
  const { current, messages } = await runDeepProbe({
    threePositionPower: true,
    threePositionMenu: true,
    threePositionMarker: "missing-marker",
  });

  assert.equal(current, "GPT-6");
  assert.equal(messages.length, 1);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high"]);
});

test("failure to restore the original model prevents the completed snapshot", async () => {
  const { current, messages } = await runDeepProbe({
    failSelection: (name, selected) => name === "GPT-6" && selected !== "GPT-6",
  });

  assert.equal(current, "Auto");
  assert.equal(messages.length, 0);
});

test("a neutral model-name button is recognized without a model aria-label", async () => {
  let menuOpen = false;
  let hideChoices = false;
  let ambiguous = false;
  let opens = 0;
  let current = "GPT-6";
  let now = 0;
  const messages = [];
  const element = (rect, extra = {}) => ({
    isConnected: true,
    parentElement: null,
    matches: () => false,
    getBoundingClientRect: () => rect,
    getAttribute: () => null,
    closest: () => null,
    querySelector: () => null,
    querySelectorAll: () => [],
    dispatchEvent: () => true,
    ...extra,
  });
  const control = element({ x: 0, y: 0, width: 140, height: 32 }, {
    outerHTML: "<button>Model</button>",
    matches: selector => selector === '[data-tone="neutral"]',
    getAttribute: name => name === "aria-expanded" ? String(menuOpen) : null,
    click() { if (!menuOpen) opens++; menuOpen = !menuOpen; },
  });
  Object.defineProperty(control, "innerText", { get: () => current });
  const options = ["GPT-6", "GPT-5.6"].map(label => element({ x: 0, y: 48, width: 160, height: 32 }, {
    innerText: label,
    getAttribute: name => name === "aria-checked" ? String(current === label) : null,
    // This variant updates the trigger immediately but keeps its menu open.
    click() { current = label; },
  }));
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("data-tone")) return ambiguous ? [control, { ...control, innerText: current }] : [control];
      if (selector.includes('[role="menuitem"]') || selector.includes("[data-model-id]")) {
        return menuOpen && !hideChoices ? options : [];
      }
      return [];
    },
    dispatchEvent(event) { if (event.key === "Escape") menuOpen = false; },
  };
  const window = {
    __web2codexDeepProbe: false,
    ipc: { postMessage(message) { messages.push(JSON.parse(message)); } },
  };
  window.top = window;
  class FakeEvent { constructor(type, options = {}) { this.type = type; Object.assign(this, options); } }
  const context = vm.createContext({
    window,
    document,
    location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent,
    PointerEvent: FakeEvent,
    Date: class extends Date { static now() { return now += 1000; } },
    setTimeout: callback => { queueMicrotask(callback); return 1; },
    clearTimeout,
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);

  await window.__web2codexModelPicker.select("GPT-6");

  assert.equal(menuOpen, false);
  assert.equal(opens, 0, "an exact current model needs no menu on response-bound pages");
  assert.equal(messages.length, 0);
  ambiguous = true;
  await window.__web2codexModelPicker.select("GPT-6");
  assert.equal(opens, 1, "ambiguous triggers require menu confirmation");
  assert.deepEqual(messages[0].picker_options, ["GPT-6", "GPT-5.6"]);
  ambiguous = false;
  window.__web2codexDeepProbe = true;
  await window.__web2codexModelPicker.select("GPT-6");
  assert.equal(opens, 2, "deep probe must still open the model menu");
  window.__web2codexDeepProbe = false;

  hideChoices = true;
  await assert.rejects(window.__web2codexModelPicker.select("GPT-5.6"), /model menu is unavailable/);
  assert.equal(menuOpen, false, "a failed open must dismiss the empty model menu");
  hideChoices = false;
  await window.__web2codexModelPicker.select("GPT-5.6");
  assert.equal(current, "GPT-5.6");
  assert.equal(menuOpen, false, "a matching trigger label must not leave the model menu open");
});
