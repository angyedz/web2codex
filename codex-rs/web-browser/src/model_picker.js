(() => {
  if (window.top !== window || location.origin !== "https://chatgpt.com") return;
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = element => {
    if (!element?.isConnected) return false;
    for (let node = element; node; node = node.parentElement) {
      if (node.matches('[hidden], [inert], [aria-hidden="true"]')) return false;
      const style = getComputedStyle(node);
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") return false;
    }
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const text = element => (element.innerText || element.textContent || "").replace(/\s+/g, " ").trim();
  const normalize = value => value.toLowerCase().replace(/[^a-z0-9]/g, "");
  const activate = element => {
    element.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", button: 0, buttons: 1 }));
    element.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", button: 0 }));
    element.click();
  };
  const canonicalEffort = value => {
    const match = (value || "").trim().match(/^(extra\s+high|xhigh|max|pro|low|light|instant|medium|standard|high|extended|низк(?:ий|ая|ое)|л[её]гк(?:ий|ая|ое)|средн(?:ий|яя|ее)|стандартн(?:ый|ая|ое)|высок(?:ий|ая|ое)|расширенн(?:ый|ая|ое))(?:\s|[›→>]|$)/i);
    if (!match) return null;
    const label = match[1].toLowerCase();
    if (["extra high", "xhigh"].includes(label)) return "xhigh";
    if (["max", "pro"].includes(label)) return "max";
    if (["low", "light", "instant"].includes(label) || /^(низк|л[её]гк)/.test(label)) return "low";
    if (["medium", "standard"].includes(label) || /^(средн|стандартн)/.test(label)) return "medium";
    return "high";
  };
  const isEffort = value => canonicalEffort(value) !== null;
  const isModel = value => /^(?:(?:chatgpt\s+)?gpt[\s\-‐‑‒–—−]*\d|latest\b|default\b|最新|최신)/i.test(value);
  const choices = () => [...document.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"], [data-model-id]')]
    .filter(visible).filter(element => isModel(text(element)))
    .filter(element => !element.parentElement?.closest('[role="menuitem"], [role="menuitemradio"], [role="option"]'));
  const waitFor = async (read, timeout) => {
    const deadline = Date.now() + timeout;
    do { const result = read(); if (result) return result; await sleep(100); } while (Date.now() < deadline);
    return null;
  };
  const findControl = () => [...document.querySelectorAll('button[data-testid="model-switcher-dropdown-button"], button[data-composer-navigation-target="reasoning"][aria-haspopup="menu"], button[aria-label*="model" i], button[aria-label*="модел" i], button[aria-label*="reasoning effort" i], button[aria-label*="усилий рассуждения" i], button[data-tone="neutral"][aria-haspopup="menu"]')]
    .filter(visible).find(button => !button.closest('[role="menu"], [role="listbox"]') && (
      !button.matches('[data-tone="neutral"]') || isEffort(text(button)) || /model|модел|reasoning|рассужд/i.test(button.getAttribute("aria-label") || "")));
  const effortRow = control => {
    const anchor = control.getBoundingClientRect();
    return [...document.querySelectorAll('[role="menuitem"], [role="button"], button, [data-effort-only="true"], [data-model-reasoning-effort-slider], [data-model-picker-power-slider]')]
      .filter(visible).filter(element => element !== control && !element.contains(control) && (
        isEffort(text(element)) || canonicalEffort(element.querySelector('[role="slider"], input[type="range"]')?.getAttribute("aria-valuetext")) ||
        (element.matches('[data-model-reasoning-effort-slider], [data-model-picker-power-slider]') && element.querySelector('[role="slider"], input[type="range"]'))))
      .filter(element => {
        const rect = element.getBoundingClientRect();
        return Math.abs(rect.x - anchor.x) < 250 && Math.abs(rect.y - anchor.y) < 200 && Math.abs(rect.y - anchor.y) > 10;
      }).sort((a, b) => {
        // The model-list header may also display "Medium". Prefer the row
        // which actually owns the slider over that similarly labelled header.
        const hasSlider = element => !!element.querySelector('[role="slider"], input[type="range"]');
        return Number(hasSlider(b)) - Number(hasSlider(a)) ||
          a.getBoundingClientRect().width * a.getBoundingClientRect().height - b.getBoundingClientRect().width * b.getBoundingClientRect().height;
      })[0];
  };
  const availableEfforts = row => {
    const efforts = effortSequence(row);
    const slider = row?.querySelector('[role="slider"], input[type="range"]');
    const min = slider?.getAttribute("aria-valuemin");
    const max = slider?.getAttribute("aria-valuemax");
    const ticks = row ? [...row.querySelectorAll('[data-selected]')] : [];
    if (!/^-?\d+$/.test(min || "") || !/^-?\d+$/.test(max || "") ||
        Number(max) - Number(min) + 1 !== ticks.length || ticks.length < 1 || ticks.length > 6) return [];
    const power = row.matches('[data-model-picker-power-slider]') &&
      !!row.querySelector('[data-orientation="horizontal"][aria-disabled="false"]');
    const locks = ticks.map(tick => tick.getAttribute("data-locked") ?? (power ? "false" : null));
    if (locks.some(lock => lock !== "true" && lock !== "false")) return [];
    return efforts.filter((effort, index) => effort && locks[index] === "false");
  };
  const effortSequence = row => {
    const slider = row?.querySelector('[role="slider"], input[type="range"]');
    const min = slider?.getAttribute("aria-valuemin");
    const max = slider?.getAttribute("aria-valuemax");
    const ticks = row ? [...row.querySelectorAll('[data-selected]')] : [];
    if (!/^-?\d+$/.test(min || "") || !/^-?\d+$/.test(max || "") ||
        Number(max) - Number(min) + 1 !== ticks.length || ticks.length < 1 || ticks.length > 6) return [];
    const labels = ticks.map(tick => [
      tick.getAttribute("aria-label"), tick.getAttribute("aria-valuetext"), tick.getAttribute("title"), text(tick),
    ].map(canonicalEffort).find(Boolean));
    // Five-position controls have a stable low→max ordering. Newer six-position
    // controls must expose each position's label; guessing could select the wrong mode.
    if (ticks.length === 5 && labels.every(label => !label)) return ["low", "medium", "high", "xhigh", "max"];
    return labels.every(Boolean) && new Set(labels).size === labels.length ? labels : [];
  };
  const open = async controlHint => {
    const control = controlHint?.isConnected ? controlHint : await waitFor(findControl, 10000);
    if (!control) throw new Error("ChatGPT model picker is unavailable");
    if (control.getAttribute("aria-expanded") !== "true") control.click();
    const surface = await waitFor(() => {
      const options = choices();
      if (options.length) return { options };
      const row = effortRow(control);
      return row ? { row } : null;
    }, 3000);
    let options = surface?.options;
    let efforts = [];
    if (!options) {
      const row = surface?.row;
      if (!row) throw new Error("ChatGPT model menu is unavailable");
      efforts = availableEfforts(row);
      // The row contains a slider. Click its label, never the slider itself.
      const modelToggle = [...document.querySelectorAll('[data-model-picker-view-toggle="true"]')].find(visible);
      const label = [...row.querySelectorAll("span, div")].filter(visible)
        .find(element => isEffort(text(element)) && !element.querySelector('[role="slider"], input[type="range"]'));
      activate(modelToggle || label || row);
      options = await waitFor(() => choices().length && choices(), 5000);
    }
    if (!options) throw new Error("ChatGPT did not show its model choices");
    return { control, options, efforts };
  };
  const close = () => { for (let i = 0; i < 4; i++) document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", bubbles: true })); };
  const selected = option => [option, ...option.querySelectorAll('[aria-checked], [aria-selected], [aria-pressed], [data-state]')]
    .some(node => node.getAttribute("aria-checked") === "true" || node.getAttribute("aria-selected") === "true" || node.getAttribute("aria-pressed") === "true" || node.getAttribute("data-state") === "checked");
  const effortValue = row => {
    const slider = row.querySelector('[role="slider"], input[type="range"]');
    const min = slider?.getAttribute("aria-valuemin");
    const max = slider?.getAttribute("aria-valuemax");
    const value = slider?.getAttribute("aria-valuenow");
    if ([min, max, value].every(part => typeof part === "string" && /^-?\d+$/.test(part))) {
      const lower = Number(min), upper = Number(max), current = Number(value);
      if ([lower, upper, current].every(Number.isInteger) && upper >= lower && current >= lower && current <= upper) {
        const sequence = effortSequence(row);
        if (sequence.length === upper - lower + 1) return sequence[current - lower] || null;
      }
    }
    const fromText = canonicalEffort(text(row));
    if (fromText) return fromText;
    return canonicalEffort(slider?.getAttribute("aria-valuetext"));
  };
  const setEffort = async requested => {
    const target = normalize(requested);
    if (!["low", "medium", "high", "xhigh", "max"].includes(target)) throw new Error(`Unsupported ChatGPT reasoning effort: ${requested}`);
    const control = await waitFor(findControl, 10000);
    if (!control) throw new Error("ChatGPT reasoning effort picker is unavailable");
    control.click();
    let row = await waitFor(() => effortRow(control), 3000);
    if (!row) { close(); throw new Error("ChatGPT did not expose its reasoning effort control"); }
    const ticks = [...row.querySelectorAll('[data-selected]')];
    const sequence = effortSequence(row);
    const selectedIndex = sequence.indexOf(target);
    if (ticks.length && (selectedIndex < 0 || !availableEfforts(row).includes(target) || ticks[selectedIndex].getAttribute("data-locked") === "true")) {
      close();
      throw new Error(`ChatGPT does not make ${requested} reasoning effort available`);
    }
    for (let attempt = 0; attempt < 6 && effortValue(row) !== target; attempt++) {
      const slider = row.querySelector('[role="slider"], input[type="range"], [tabindex]') || row;
      // ChatGPT's ARIA slider can be hidden; its menu item handles keyboard input.
      const sliderControl = slider.closest?.('[role="menuitem"]') || slider;
      sliderControl.focus();
      const current = effortValue(row);
      if (!current) { close(); throw new Error("ChatGPT did not expose its current reasoning effort"); }
      const currentIndex = sequence.indexOf(current);
      const direction = currentIndex >= 0 && selectedIndex >= 0
        ? selectedIndex > currentIndex ? "ArrowRight" : "ArrowLeft"
        : ["low", "medium", "high", "xhigh", "max"].indexOf(target) > ["low", "medium", "high", "xhigh", "max"].indexOf(current) ? "ArrowRight" : "ArrowLeft";
      sliderControl.dispatchEvent(new KeyboardEvent("keydown", { key: direction, code: direction, bubbles: true, cancelable: true }));
      sliderControl.dispatchEvent(new KeyboardEvent("keyup", { key: direction, code: direction, bubbles: true, cancelable: true }));
      await sleep(200);
      row = effortRow(control) || row;
    }
    const applied = effortValue(effortRow(control) || row);
    close();
    if (applied !== target) throw new Error(`ChatGPT did not apply ${requested} reasoning effort`);
  };
  const select = async requested => {
    const target = normalize(requested);
    let menu = await open();
    let matches = menu.options.filter(option => normalize(text(option)) === target);
    if (matches.length !== 1) { close(); throw new Error(`ChatGPT does not expose one exact picker choice for ${requested}`); }
    if (matches[0].getAttribute("aria-disabled") === "true" || matches[0].getAttribute("data-disabled") === "true") {
      close(); throw new Error(`ChatGPT does not make model ${requested} available`);
    }
    if (selected(matches[0])) { close(); return; }
    activate(matches[0]);
    await sleep(400);
    const control = findControl();
    if (control && !isEffort(text(control)) && normalize(text(control)) === target) return;
    menu = await open(menu.control);
    matches = menu.options.filter(option => normalize(text(option)) === target);
    const confirmed = matches.length === 1 && selected(matches[0]);
    close();
    if (!confirmed) throw new Error(`ChatGPT did not confirm model ${requested}`);
  };
  window.__web2codexModelPicker = { select, setEffort };
  window.__web2codexCapabilityProbeComplete = (async () => {
    let options = [];
    let reasoningByModel = {};
    let original = null;
    try {
      // ChatGPT can render its controls before they are interactive. Retry
      // for up to a minute while the authenticated Temporary Chat hydrates.
      for (let attempt = 0; attempt < 30 && options.length === 0; attempt++) {
        try {
          const menu = await open();
          options = [...new Set(menu.options.map(text).filter(value => value.length <= 120))].slice(0, 40);
          const active = menu.options.filter(selected);
          if (active.length === 1) {
            original = text(active[0]);
            if (menu.efforts.length) reasoningByModel[original] = menu.efforts;
          }
          close();
          if (window.__web2codexDeepProbe && original) {
            for (const name of options) {
              if (name === original) continue;
              try {
                await select(name);
                const control = await waitFor(findControl, 5000);
                if (!control) continue;
                control.click();
                const row = await waitFor(() => effortRow(control), 3000);
                const efforts = availableEfforts(row);
                if (efforts.length) reasoningByModel[name] = efforts;
              } catch (_) {
                // A locked or changed picker option is not evidence of available levels.
              } finally {
                close();
              }
            }
            try { await select(original); } catch (_) { /* The next request verifies its model again. */ }
          }
        } catch (_) {
          close();
          await sleep(2000);
        }
      }
    } catch (_) {
      // An empty snapshot indicates that the live picker could not be read.
    } finally {
      close();
      window.ipc.postMessage(JSON.stringify({ type: "account-capabilities", probe_version: 4, picker_options: options, reasoning_by_model: reasoningByModel }));
    }
  })();
})();
