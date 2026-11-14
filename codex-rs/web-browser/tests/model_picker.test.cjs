const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

test("Russian effort labels can be changed through the reasoning control", async () => {
  const levels = ["Лёгкое", "Средний", "Высокий"];
  let level = 1;
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
  const slider = element({}, {
    focus() {},
    dispatchEvent(event) {
      if (event.type === "keydown") level += event.key === "ArrowLeft" ? -1 : 1;
      return true;
    },
  });
  const row = element({ x: 0, y: 50, width: 100, height: 30 }, {
    querySelector: () => slider,
  });
  Object.defineProperty(row, "innerText", { get: () => levels[level] });
  const control = element({ x: 0, y: 0, width: 100, height: 30 }, {
    click() {},
    closest: () => null,
  });
  const option = element({ x: 0, y: 100, width: 100, height: 30 }, {
    innerText: "GPT-6",
    getAttribute: name => name === "aria-checked" ? "true" : null,
  });
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("[data-model-id]")) return [option];
      if (selector.includes("data-composer-navigation-target")) return [control];
      if (selector.includes("data-effort-only")) return [row];
      return [];
    },
    dispatchEvent() {},
  };
  const window = { ipc: { postMessage() {} } };
  window.top = window;
  class FakeEvent {
    constructor(type, options = {}) { this.type = type; Object.assign(this, options); }
  }
  const context = vm.createContext({
    window, document, location: { origin: "https://chatgpt.com" },
    getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: "1" }),
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent, setTimeout, clearTimeout,
  });
  const script = fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8");
  vm.runInContext(script, context);
  await window.__web2codexCapabilityProbeComplete;
  await window.__web2codexModelPicker.setEffort("low");
  assert.equal(levels[level], "Лёгкое");
});

test("the neutral picker opens its model list and changes an ARIA slider", async () => {
  let modelList = false;
  let selected = 1;
  let highLocked = true;
  let sixPositionControl = false;
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
      return { "aria-valuemin": "0", "aria-valuemax": sixPositionControl ? "5" : "4", "aria-valuenow": String(selected) }[name] ?? null;
    },
  });
  const control = element({ x: 0, y: 0, width: 100, height: 30 }, {
    innerText: "Medium", click() { modelList = false; }, closest: () => null,
    matches: selector => selector === '[data-tone="neutral"]',
  });
  const sliderContainer = element({ x: 0, y: 50, width: 100, height: 30 }, {
    matches: selector => selector.includes("data-model-picker-power-slider"),
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]'
      ? Array.from({ length: sixPositionControl ? 6 : 5 }, (_, index) => ({
        getAttribute: name => name === "data-locked" && index === 2 && highLocked ? "true" :
          name === "data-locked" ? "false" :
          name === "aria-label" && sixPositionControl ? ["Instant", "Medium", "High", "Extra High", "Pro", "unrecognized"][index] : null,
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
      if (selector.includes("[data-model-id]")) return modelList ? [option] : [];
      if (selector.includes("data-composer-navigation-target")) return [control];
      if (selector.includes("data-effort-only")) return [toggle, sliderContainer];
      if (selector.includes("data-model-picker-view-toggle")) return [toggle];
      return [];
    },
    dispatchEvent() {},
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
});

test("the initial probe detects levels per model and restores the original selection", async () => {
  let view = "closed";
  let current = "GPT-6";
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
    getAttribute: name => ({ "aria-valuemin": "0", "aria-valuemax": "4", "aria-valuenow": "1" })[name] ?? null,
  });
  const row = element({ x: 0, y: 50, width: 100, height: 30 }, {
    matches: selector => selector.includes("data-model-picker-power-slider"),
    querySelector: () => slider,
    querySelectorAll: selector => selector === '[data-selected]'
      ? Array.from({ length: 5 }, (_, index) => ({
        getAttribute: name => name === "data-locked" && current === "GPT-5.6 Sol" && index >= 3 ? "true" : "false",
      })) : [],
  });
  const toggle = element({ x: 0, y: 30, width: 100, height: 20 }, { click() { view = "models"; } });
  const options = ["GPT-6", "GPT-5.6 Sol"].map(name => element(
    { x: 0, y: 100, width: 100, height: 30 }, {
      innerText: name,
      getAttribute: attr => attr === "aria-checked" ? String(current === name) : null,
      click() { current = name; view = "effort"; },
    },
  ));
  const document = {
    querySelectorAll(selector) {
      if (selector.includes("[data-model-id]")) return view === "models" ? options : [];
      if (selector.includes("data-composer-navigation-target")) return [control];
      if (selector.includes("data-effort-only")) return view === "effort" ? [row] : [];
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
    KeyboardEvent: FakeEvent, PointerEvent: FakeEvent, setTimeout, clearTimeout });
  vm.runInContext(fs.readFileSync(path.join(__dirname, "../src/model_picker.js"), "utf8"), context);
  await window.__web2codexCapabilityProbeComplete;
  assert.equal(current, "GPT-6");
  assert.deepEqual(messages[0].reasoning_by_model["GPT-6"], ["low", "medium", "high", "xhigh", "max"]);
  assert.deepEqual(messages[0].reasoning_by_model["GPT-5.6 Sol"], ["low", "medium", "high"]);
});
