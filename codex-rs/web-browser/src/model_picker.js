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
  const normalize = value => value.normalize("NFKC").toLowerCase().replace(/[\s\-‐‑‒–—−]+/g, "");
  const activate = element => {
    element.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", button: 0, buttons: 1 }));
    element.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", button: 0 }));
    element.click();
  };
  const canonicalEffort = value => {
    const match = (value || "").trim().match(/^(extra[\s-]+high|xhigh|max(?:imum)?|pro|low|light|instant|medium|standard|high|extended|очень\s+высок(?:ий|ая|ое)|максимальн(?:ый|ая|ое)|низк(?:ий|ая|ое)|л[её]гк(?:ий|ая|ое)|мгновенн(?:ый|ая|ое)|средн(?:ий|яя|ее)|стандартн(?:ый|ая|ое)|высок(?:ий|ая|ое)|расширенн(?:ый|ая|ое))(?=[\s,.;:›→>]|$)/i);
    if (!match) return null;
    const label = match[1].toLowerCase().replace(/[\s-]+/g, " ");
    if (["extra high", "xhigh"].includes(label) || label.startsWith("очень")) return "xhigh";
    if (["max", "maximum", "pro"].includes(label) || label.startsWith("максимальн")) return "max";
    if (["low", "light", "instant"].includes(label) || /^(низк|л[её]гк|мгновенн)/.test(label)) return "low";
    if (["medium", "standard"].includes(label) || /^(средн|стандартн)/.test(label)) return "medium";
    return "high";
  };
  const effortFromText = value => canonicalEffort(value) ||
    [...String(value || "").matchAll(/(?:^|[^a-zа-яё])(?=[a-zа-яё])/gi)]
      .map(match => canonicalEffort(String(value).slice(match.index + match[0].length))).find(Boolean) || null;
  const isEffort = value => canonicalEffort(value) !== null;
  const isModel = value => /^(?:(?:chatgpt\s+)?gpt[\s\-‐‑‒–—−]*\d|o[\s\-‐‑‒–—−]*\d+(?:\b|[\s\-‐‑‒–—−])|(?:luna|sol|think|latest|default|auto)\b|по умолчанию(?:\s|$)|最新|최신)/i.test(value) ||
    /^\d+(?:\.\d+)?(?:\s+(?:sol|astra|pro))*$/i.test(value);
  const modelLabel = option => {
    const primary = option.querySelector('[data-menu-row-content] span:first-child');
    const label = primary ? text(primary) : "";
    return isModel(label) ? label : text(option);
  };
  const available = option => !option.disabled && option.getAttribute("aria-disabled") !== "true" &&
    !["", "true"].includes(option.getAttribute("data-disabled")) && option.getAttribute("data-locked") !== "true";
  const labelsInMenu = menu => [...new Set(menu.options.filter(available).map(modelLabel)
    .filter(label => label && label.length <= 120))].slice(0, 40);
  const publish = (options, reasoningByModel) => window.ipc.postMessage(JSON.stringify({
    type: "account-capabilities", probe_version: 5, picker_options: options, reasoning_by_model: reasoningByModel,
  }));
  const choices = () => [...document.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"], [data-model-id]')]
    .filter(visible).filter(element => !!element.getAttribute("data-model-id") || isModel(modelLabel(element)))
    .filter(element => !element.parentElement?.closest('[role="menuitem"], [role="menuitemradio"], [role="option"]'));
  const waitFor = async (read, timeout) => {
    const deadline = Date.now() + timeout;
    do { const result = read(); if (result) return result; await sleep(100); } while (Date.now() < deadline);
    return null;
  };
  const modelControls = () => [...document.querySelectorAll('button[data-testid="model-switcher-dropdown-button"], button[aria-label*="model" i], button[aria-label*="модел" i], button[data-tone="neutral"][aria-haspopup="menu"]')]
    .filter(visible).filter(button => !button.closest('[role="menu"], [role="listbox"]') &&
      !isEffort(text(button)) && (isModel(text(button)) || /model|модел/i.test(button.getAttribute("aria-label") || "")));
  const findModelControl = () => modelControls()[0];
  const explicitEffortControl = button => button.matches('[data-composer-navigation-target="reasoning"]') ||
    button.getAttribute("data-selected-reasoning-effort") !== null ||
    button.getAttribute("data-codex-intelligence-trigger") === "true";
  const findEffortControl = () => {
    const controls = [...document.querySelectorAll('button[data-composer-navigation-target="reasoning"][aria-haspopup="menu"], button[data-selected-reasoning-effort][aria-haspopup="menu"], button[data-codex-intelligence-trigger="true"][aria-haspopup="menu"], button[aria-label*="reasoning effort" i], button[aria-label*="усилий рассуждения" i], button[data-tone="neutral"][aria-haspopup="menu"], button, [role="button"]')]
      .filter(visible).filter(button => !button.closest('[role="menu"], [role="listbox"]'));
    return controls.find(explicitEffortControl) ||
      controls.find(button => /reasoning|рассужд/i.test(button.getAttribute("aria-label") || "")) ||
      controls.find(button => isEffort(text(button)) &&
        (button.hasAttribute("aria-haspopup") || button.closest("form")));
  };
  const findControl = () => findModelControl() || findEffortControl();
  const effortControlResolver = initial => {
    let control = initial;
    let dedicated = !!initial && (explicitEffortControl(initial) || isEffort(text(initial)) ||
      /reasoning|рассужд/i.test(initial.getAttribute("aria-label") || ""));
    return () => {
      const effort = findEffortControl();
      if (effort) {
        dedicated = true;
        control = effort;
      } else if (!control?.isConnected || !visible(control)) {
        control = dedicated ? null : findModelControl();
      }
      return control;
    };
  };
  const controlClosed = control => control.getAttribute("aria-expanded") === "false" ||
    control.getAttribute("data-state") === "closed";
  const controlOpen = control => control.getAttribute("aria-expanded") === "true" ||
    control.getAttribute("data-state") === "open";
  const inOwnedSurface = (control, element) => {
    const ids = (control.getAttribute("aria-controls") || "").split(/\s+/).filter(Boolean);
    const controlId = control.getAttribute("id");
    for (let node = element; node; node = node.parentElement) {
      if (ids.includes(node.getAttribute("id"))) return true;
      if (!ids.length && controlId && node.matches('[role="menu"], [role="listbox"], [role="dialog"]')) {
        const labels = (node.getAttribute("aria-labelledby") || "").split(/\s+/).filter(Boolean);
        if (labels.length) return labels.includes(controlId);
      }
    }
    return !ids.length;
  };
  const hasSlider = element => !!element?.querySelector('[role="slider"], input[type="range"]');
  const sliderMarker = '[data-model-picker-power-slider], [data-model-reasoning-effort-slider], [data-effort-only="true"]';
  const pointerOpen = control => control.dispatchEvent(new PointerEvent("pointerdown", {
    bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", isPrimary: true, button: 0, buttons: 1,
  }));
  const effortRow = control => {
    if (!control || controlClosed(control)) return null;
    // The semantic range input may be zero-width and aria-hidden. Its visible
    // marked container is the structural evidence, including portal siblings.
    const marked = [...document.querySelectorAll(sliderMarker)].filter(visible)
      .filter(element => element.matches(sliderMarker) && hasSlider(element));
    const owned = marked.filter(element => inOwnedSurface(control, element));
    if (owned.length === 1) return owned[0];
    if (!owned.length && marked.length === 1 && controlOpen(control) &&
        !marked[0].closest?.('[role="menu"], [role="listbox"], [role="dialog"]')) return marked[0];
    const anchor = control.getBoundingClientRect();
    const hasVerifiedPowerSlider = element => {
      const slider = element.querySelector('[role="slider"], input[type="range"]');
      const min = slider?.getAttribute("aria-valuemin");
      const max = slider?.getAttribute("aria-valuemax");
      const value = slider?.getAttribute("aria-valuenow");
      if (!slider || !available(slider) || ![min, max, value].every(part => /^-?\d+$/.test(part || "")) ||
          Number(max) - Number(min) + 1 !== 3) return false;
      const sequence = ["low", "medium", "high"];
      const currentEffort = effortFromText(slider.getAttribute("aria-valuetext")) || effortFromText(text(element));
      return sequence[Number(value) - Number(min)] === currentEffort;
    };
    return [...document.querySelectorAll('[role="menu"], [role="group"], [role="dialog"], [role="menuitem"], [role="button"], button, [data-testid="composer-intelligence-picker-content"], [data-effort-only="true"], [data-model-reasoning-effort-slider], [data-model-picker-power-slider]')]
      .filter(visible).filter(element => element !== control && !element.contains?.(control) && inOwnedSurface(control, element) && (
        isEffort(text(element)) || canonicalEffort(element.querySelector('[role="slider"], input[type="range"]')?.getAttribute("aria-valuetext")) ||
        hasVerifiedPowerSlider(element) ||
        (element.matches('[data-model-reasoning-effort-slider], [data-model-picker-power-slider]') && element.querySelector('[role="slider"], input[type="range"]'))))
      .filter(element => {
        if (control.getAttribute("aria-controls")) return true;
        const rect = element.getBoundingClientRect();
        return Math.abs(rect.x - anchor.x) < 250 && Math.abs(rect.y - anchor.y) < 200 && Math.abs(rect.y - anchor.y) > 10;
      }).sort((a, b) => {
        // The model-list header may also display "Medium". Prefer the row
        // which actually owns the slider over that similarly labelled header.
        return Number(hasSlider(b)) - Number(hasSlider(a)) ||
          a.getBoundingClientRect().width * a.getBoundingClientRect().height - b.getBoundingClientRect().width * b.getBoundingClientRect().height;
      })[0];
  };
  const effortMenuOptions = control => {
    if (!control || controlClosed(control)) return [];
    const anchor = control.getBoundingClientRect();
    const candidates = [...document.querySelectorAll('button, [role="menuitem"], [role="menuitemradio"], [role="option"], [role="radio"]')]
      .filter(visible)
      .filter(element => element !== control && !element.contains?.(control) && inOwnedSurface(control, element) &&
        !hasSlider(element) && !element.matches('[data-model-picker-view-toggle="true"]') &&
        !element.closest?.('[data-model-picker-view-toggle="true"]'))
      .map(element => ({ element, effort: canonicalEffort(text(element)) }))
      .filter(option => option.effort)
      .filter(({ element }) => {
        if (control.getAttribute("aria-controls")) return true;
        const rect = element.getBoundingClientRect();
        return Math.abs(rect.x - anchor.x) < 300 &&
          Math.abs(rect.y - anchor.y) < 320 &&
          Math.abs(rect.y - anchor.y) > 10;
      });
    const outermost = candidates.filter(option => !candidates.some(parent =>
      parent !== option && parent.element.contains?.(option.element)));
    const unique = new Map();
    for (const option of outermost) if (!unique.has(option.effort)) unique.set(option.effort, option);
    return [...unique.values()];
  };
  const openEffortSurface = async (control, timeout = 3000, resolveControl = effortControlResolver(control)) => {
    const read = () => {
      control = resolveControl();
      if (!control) return null;
      const row = effortRow(control);
      if (hasSlider(row)) return { control, row, menuOptions: [] };
      const menuOptions = effortMenuOptions(control);
      return menuOptions.length ? { control, row: null, menuOptions } : null;
    };
    let surface = read();
    if (surface) return surface;
    let previous = null;
    for (let attempt = 0; attempt < 3; attempt++) {
      control = await waitFor(resolveControl, timeout);
      if (!control) return null;
      if (controlOpen(control)) await close(control);
      control = await waitFor(resolveControl, timeout);
      if (!control) return null;
      if (!controlOpen(control)) {
        if (previous === control) pointerOpen(control);
        else control.click();
      }
      previous = control;
      surface = await waitFor(read, Math.min(timeout, 1000));
      if (surface) return surface;
    }
    return null;
  };
  const readEffortMenuCapabilities = control => {
    const options = effortMenuOptions(control);
    return options.length
      ? [...new Set(options.filter(option => available(option.element)).map(option => option.effort))]
      : null;
  };
  // `null` means the slider was not read reliably. An empty array is a valid
  // observation: the slider itself or every labeled level is locked.
  const readEffortCapabilities = row => {
    if (!row) return null;
    const slider = row?.querySelector('[role="slider"], input[type="range"]');
    if (slider && !available(slider)) return [];
    const efforts = effortSequence(row);
    const min = slider?.getAttribute("aria-valuemin");
    const max = slider?.getAttribute("aria-valuemax");
    const ticks = row ? [...row.querySelectorAll('[data-selected]')] : [];
    const identifiedSlider = row.matches('[data-model-picker-power-slider], [data-model-reasoning-effort-slider], [data-effort-only="true"]') &&
      !!slider && available(slider);
    const positionCount = /^-?\d+$/.test(min || "") && /^-?\d+$/.test(max || "")
      ? Number(max) - Number(min) + 1
      : 0;
    // ChatGPT's current composer power menu exposes three unlabeled positions
    // and reports the selected one as "Medium, 2 of 3". An explicit effort
    // slider marker plus that matching value is enough to identify its
    // low→medium→high sequence, even when WebView2 omits the tick elements.
    if (positionCount === 3 && ticks.length === 0 && efforts.length === 3) {
      return efforts;
    }
    if (!/^-?\d+$/.test(min || "") || !/^-?\d+$/.test(max || "") ||
        Number(max) - Number(min) + 1 !== ticks.length || ticks.length < 1 || ticks.length > 6 ||
        efforts.length !== ticks.length) return null;
    const verifiedThreePosition = positionCount === 3 && efforts.length === 3;
    const locks = ticks.map(tick => !available(tick) ? "true" : tick.getAttribute("data-locked") ??
      (identifiedSlider || verifiedThreePosition ? "false" : null));
    if (locks.some(lock => lock !== "true" && lock !== "false")) return null;
    return efforts.filter((effort, index) => effort && locks[index] === "false");
  };
  const availableEfforts = row => readEffortCapabilities(row) || [];
  const effortSequence = row => {
    const slider = row?.querySelector('[role="slider"], input[type="range"]');
    if (!slider || !available(slider)) return [];
    const min = slider?.getAttribute("aria-valuemin");
    const max = slider?.getAttribute("aria-valuemax");
    const ticks = row ? [...row.querySelectorAll('[data-selected]')] : [];
    const identifiedSlider = row?.matches('[data-model-picker-power-slider], [data-model-reasoning-effort-slider], [data-effort-only="true"]') &&
      !!slider;
    if (/^-?\d+$/.test(min || "") && /^-?\d+$/.test(max || "") &&
        Number(max) - Number(min) + 1 === 3) {
      const sequence = ["low", "medium", "high"];
      const current = slider.getAttribute("aria-valuenow");
      const currentIndex = /^-?\d+$/.test(current || "") ? Number(current) - Number(min) : -1;
      const currentEffort = effortFromText(slider.getAttribute("aria-valuetext")) || effortFromText(text(row));
      if (sequence[currentIndex] === currentEffort || (identifiedSlider && !currentEffort)) return sequence;
    }
    if (!/^-?\d+$/.test(min || "") || !/^-?\d+$/.test(max || "") ||
        Number(max) - Number(min) + 1 !== ticks.length || ticks.length < 1 || ticks.length > 6) return [];
    const labels = ticks.map(tick => [
      tick.getAttribute("aria-label"), tick.getAttribute("aria-valuetext"), tick.getAttribute("title"), text(tick),
    ].map(canonicalEffort).find(Boolean));
    // The reference picker maps indices 0–4 to Instant through Pro.
    // A sixth position must be identified explicitly; do not guess its meaning.
    if (labels.every(label => !label) && (ticks.length === 5 || (identifiedSlider && ticks.length === 4))) {
      return ["low", "medium", "high", "xhigh", "max"].slice(0, ticks.length);
    }
    return labels.every(Boolean) && new Set(labels).size === labels.length ? labels : [];
  };
  const open = async (controlHint, {
    controlTimeout = 10000, surfaceAttempts = 12, surfaceTimeout = 750, modelOptionsTimeout = 5000,
  } = {}) => {
    let control = controlHint?.isConnected ? controlHint : await waitFor(() => findModelControl() || findControl(), controlTimeout);
    if (!control) throw new Error("ChatGPT model picker is unavailable");
    const readSurface = () => {
      const options = controlClosed(control) ? [] : choices();
      const row = effortRow(control);
      const effortMenu = effortMenuOptions(control);
      if (options.length || row || effortMenu.length) return { options, row, effortMenu };
      return null;
    };
    let surface = readSurface();
    // The composer can appear before React has attached its click handlers.
    // Re-find the control after hydration instead of trusting the first click.
    for (let attempt = 0; !surface && attempt < surfaceAttempts; attempt++) {
      control = findControl() || control;
      if (!controlOpen(control)) control.click();
      surface = await waitFor(readSurface, surfaceTimeout);
      if (!surface && !controlOpen(control)) {
        pointerOpen(control);
        surface = await waitFor(readSurface, surfaceTimeout);
      }
    }
    let options = surface?.options?.length ? surface.options : null;
    let efforts = [];
    let effortVerified = false;
    if (surface?.row) {
      const observed = readEffortCapabilities(surface.row) ?? readEffortMenuCapabilities(control);
      if (observed !== null) {
        efforts = observed;
        effortVerified = true;
      }
    }
    if (!effortVerified && surface?.effortMenu?.length) {
      efforts = readEffortMenuCapabilities(control) || [];
      effortVerified = true;
    }
    if (!options) {
      const row = surface?.row;
      if (!row) {
        const menus = [...document.querySelectorAll('[role="menu"], [role="menuitem"], [data-model-picker-power-slider], [data-model-reasoning-effort-slider]')];
        throw new Error(`ChatGPT model menu is unavailable (control=${control.outerHTML.slice(0, 700)}, menus=${menus.length}, visible=${menus.filter(visible).length}, focus=${document.activeElement?.tagName}, visibility=${document.visibilityState})`);
      }
      const observed = readEffortCapabilities(row);
      if (observed !== null) {
        efforts = observed;
        effortVerified = true;
      }
      const modelToggle = [...document.querySelectorAll('[data-model-picker-view-toggle="true"]')].find(visible);
      if (surface?.effortMenu?.length && !modelToggle) {
        throw new Error("ChatGPT showed reasoning choices instead of its model choices");
      }
      // The row contains a slider. Click its model-view toggle or label, never
      // the slider itself or one of the available reasoning choices.
      const label = [...row.querySelectorAll("span, div")].filter(visible)
        .find(element => isEffort(text(element)) && !element.querySelector('[role="slider"], input[type="range"]'));
      activate(modelToggle || label || row);
      options = await waitFor(() => choices().length && choices(), modelOptionsTimeout);
    }
    if (!options) throw new Error("ChatGPT did not show its model choices");
    return { control, options, efforts, effortVerified };
  };
  const pickerOpen = control => !!control?.isConnected && !controlClosed(control) && (
    controlOpen(control) || !!effortRow(control) || effortMenuOptions(control).length > 0 ||
    choices().some(option => inOwnedSurface(control, option)));
  const close = async controlHint => {
    const opened = () => {
      const controls = [...new Set([controlHint, findModelControl(), findEffortControl()].filter(Boolean))];
      return controls.find(control => pickerOpen(control) && controlOpen(control)) || controls.find(pickerOpen);
    };
    if (!opened()) return;
    for (let i = 0; i < 4; i++) (document.activeElement || document).dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", bubbles: true, cancelable: true }));
    if (await waitFor(() => !opened(), 500)) return;
    // Some picker implementations ignore synthetic Escape. Only toggle a
    // control which still owns an open surface, and verify its dismissal.
    opened()?.click();
    if (await waitFor(() => !opened(), 500)) return;
    const control = opened();
    if (control && controlOpen(control)) pointerOpen(control);
    if (await waitFor(() => !opened(), 500)) return;
    throw new Error("ChatGPT did not close its picker menu");
  };
  const withPickerCleanup = operation => async requested => {
    let failure;
    try {
      return await operation(requested);
    } catch (error) {
      failure = error;
      throw error;
    } finally {
      try { await close(); }
      catch (error) {
        if (failure) throw new Error(`${failure.message}; ${error.message}`);
        throw error;
      }
    }
  };
  const selectionNodes = option => [option, ...option.querySelectorAll('[aria-checked], [aria-selected], [aria-pressed], [data-state]')];
  const selected = option => selectionNodes(option)
    .some(node => node.getAttribute("aria-checked") === "true" || node.getAttribute("aria-selected") === "true" || node.getAttribute("aria-pressed") === "true" || node.getAttribute("data-state") === "checked");
  const menuConfirmsEffort = (control, options, target) => {
    const choice = options.find(option => option.effort === target);
    if (!choice || !available(choice.element)) return false;
    const label = canonicalEffort(control.getAttribute("data-selected-reasoning-effort")) || canonicalEffort(text(control));
    if (label && label !== target) return false;
    const active = options.filter(option => selected(option.element));
    if (active.length) return active.length === 1 && active[0] === choice;
    const hasSelectionState = options.some(option => selectionNodes(option.element).some(node =>
      ["aria-checked", "aria-selected", "aria-pressed"].some(name => ["true", "false"].includes(node.getAttribute(name))) ||
      ["checked", "unchecked"].includes(node.getAttribute("data-state"))));
    return !hasSelectionState && label === target;
  };
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
  const setEffort = withPickerCleanup(async requested => {
    const normalized = normalize(requested);
    const target = normalized === "instant" ? "low" : normalized;
    if (!["low", "medium", "high", "xhigh", "max"].includes(target)) throw new Error(`Unsupported ChatGPT reasoning effort: ${requested}`);
    let control = await waitFor(findEffortControl, 1000) || await waitFor(findControl, 10000);
    if (!control) throw new Error("ChatGPT reasoning effort picker is unavailable");
    const resolveControl = effortControlResolver(control);
    const reopen = () => openEffortSurface(control, 3000, resolveControl);
    const surface = await reopen();
    control = surface?.control || control;
    const menuOptions = surface?.menuOptions;
    if (menuOptions?.length) {
      const choice = menuOptions.find(option => option.effort === target);
      if (!choice || !available(choice.element)) {
        throw new Error(`ChatGPT does not make ${requested} reasoning effort available`);
      }
      if (!menuConfirmsEffort(control, menuOptions, target)) activate(choice.element);
      // Some menu implementations update their option or trigger before the
      // setting is committed. Verify a newly opened surface after dismissal.
      await sleep(100);
      await close(control);
      const confirmed = await reopen();
      control = confirmed?.control || control;
      const persisted = confirmed?.menuOptions?.length && await waitFor(() => {
        control = resolveControl();
        return control && menuConfirmsEffort(control, effortMenuOptions(control), target);
      }, 3000);
      if (!persisted) throw new Error(`ChatGPT did not retain ${requested} reasoning effort after closing its menu`);
      return;
    }
    let row = surface?.row;
    if (!row) throw new Error("ChatGPT did not expose its reasoning effort control");
    const ticks = [...row.querySelectorAll('[data-selected]')];
    const sequence = effortSequence(row);
    const selectedIndex = sequence.indexOf(target);
    if (selectedIndex < 0 || !availableEfforts(row).includes(target) || ticks[selectedIndex]?.getAttribute("data-locked") === "true") {
      throw new Error(`ChatGPT does not make ${requested} reasoning effort available`);
    }
    const foregroundInput = () => document.visibilityState !== "hidden" && document.hasFocus?.() !== false;
    let nativeError = null;
    const refreshRow = async input => {
      control = resolveControl();
      row = control && effortRow(control);
      if (!hasSlider(row)) {
        const reopened = await reopen();
        control = reopened?.control || control;
        row = reopened?.row;
      }
      if (!row) throw new Error(`ChatGPT did not expose its reasoning effort control after ${input}${nativeError ? `: ${nativeError}` : ""}`);
    };
    let nativeAttempted = false;
    const tryNative = async allowFocus => {
      const range = row.querySelector('[role="slider"], input[type="range"]');
      if (nativeAttempted || sequence.length !== 3 || effortSequence(row).join() !== sequence.join() ||
          !availableEfforts(row).includes(target) || range?.getAttribute("aria-valuemin") !== "0" ||
          range.getAttribute("aria-valuemax") !== "2" || !effortValue(row) || effortValue(row) === target) return;
      nativeAttempted = true;
      const requestId = `effort-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      window.ipc.postMessage(JSON.stringify({ type: "set-power-slider", request_id: requestId, index: selectedIndex, allow_focus: allowFocus }));
      const deadline = Date.now() + 5000;
      let nativeResult = null;
      while (Date.now() < deadline) {
        const currentControl = resolveControl();
        const refreshed = currentControl && effortRow(currentControl);
        if (hasSlider(refreshed)) {
          control = currentControl;
          row = refreshed;
        }
        nativeResult = window.__web2codexPowerSliderResults?.[requestId] || null;
        if ((hasSlider(refreshed) && effortValue(refreshed) === target) || (nativeResult && !nativeResult.ok)) break;
        await sleep(100);
      }
      if (window.__web2codexPowerSliderResults) delete window.__web2codexPowerSliderResults[requestId];
      if (nativeResult && !nativeResult.ok) {
        nativeError = nativeResult.error || "native slider rejected the value";
      }
      await refreshRow("native input");
    };
    if (foregroundInput()) await tryNative(true);
    for (let attempt = 0; attempt < 6 && effortValue(row) !== target; attempt++) {
      const slider = row.querySelector('[role="slider"], input[type="range"], [tabindex]') || row;
      const menuItem = slider.closest?.('[role="menuitem"]');
      const current = effortValue(row);
      if (!current) throw new Error("ChatGPT did not expose its current reasoning effort");
      if (effortSequence(row).join() !== sequence.join() || !availableEfforts(row).includes(target)) {
        throw new Error("ChatGPT changed its reasoning effort range during selection");
      }
      const currentIndex = sequence.indexOf(current);
      const direction = currentIndex >= 0 && selectedIndex >= 0
        ? selectedIndex > currentIndex ? "ArrowRight" : "ArrowLeft"
        : ["low", "medium", "high", "xhigh", "max"].indexOf(target) > ["low", "medium", "high", "xhigh", "max"].indexOf(current) ? "ArrowRight" : "ArrowLeft";
      const targets = [...new Set([menuItem, slider].filter(Boolean))];
      for (const targetControl of targets) {
        // DOM focus stays inside the WebView and lets ChatGPT's slider handle
        // keyboard input while the browser window remains hidden. Native
        // foreground input remains gated separately in tryNative(true).
        targetControl.focus?.();
        targetControl.dispatchEvent(new KeyboardEvent("keydown", { key: direction, code: direction, bubbles: true, cancelable: true }));
        targetControl.dispatchEvent(new KeyboardEvent("keyup", { key: direction, code: direction, bubbles: true, cancelable: true }));
        await sleep(200);
        await refreshRow("keyboard input");
        if (effortValue(row) !== current) break;
      }
      if (effortValue(row) === current) {
        const tick = row.querySelectorAll('[data-selected]')[selectedIndex];
        if (tick && visible(tick) && available(tick) && typeof tick.click === "function") {
          activate(tick);
          await sleep(200);
          await refreshRow("tick input");
        }
      }
      if (effortValue(row) === current) break;
    }
    if (effortValue(row) !== target && sequence.length === 3) {
      const thumb = row.querySelector('[role="slider"], input[type="range"]');
      const thumbRect = thumb?.getBoundingClientRect();
      const sliderRoot = thumb && [thumb.parentElement, thumb.parentElement?.parentElement, thumb.parentElement?.parentElement?.parentElement]
        .find(element => element?.getAttribute("data-orientation") === "horizontal" &&
          element.querySelector?.('[role="slider"], input[type="range"]') === thumb &&
          element.getBoundingClientRect().width > thumbRect.width * 2);
      const rootRect = sliderRoot?.getBoundingClientRect();
      if (sliderRoot && thumbRect?.width > 0 && rootRect?.width > thumbRect.width * 2 && rootRect.height > 0) {
        const clientX = rootRect.x + thumbRect.width / 2 +
          (rootRect.width - thumbRect.width) * selectedIndex / (sequence.length - 1);
        const clientY = rootRect.y + rootRect.height / 2;
        const pointer = { bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse", isPrimary: true,
          button: 0, buttons: 1, clientX, clientY };
        sliderRoot.dispatchEvent(new PointerEvent("pointerdown", pointer));
        sliderRoot.dispatchEvent(new PointerEvent("pointermove", pointer));
        sliderRoot.dispatchEvent(new PointerEvent("pointerup", { ...pointer, buttons: 0 }));
        await sleep(200);
        await refreshRow("pointer input");
      }
    }
    if (effortValue(row) !== target) await tryNative(false);
    const applied = effortValue(row);
    if (applied !== target) throw new Error(`ChatGPT did not apply ${requested} reasoning effort${nativeError ? `: ${nativeError}` : ""}`);
    await close(control);
    const confirmed = await reopen();
    const persisted = confirmed?.row && effortSequence(confirmed.row).join() === sequence.join() &&
      availableEfforts(confirmed.row).includes(target) && effortValue(confirmed.row) === target;
    if (!persisted) throw new Error(`ChatGPT did not retain ${requested} reasoning effort after closing its menu`);
  });
  const select = withPickerCleanup(async requested => {
    const target = normalize(requested);
    if (window.__web2codexDeepProbe === false) {
      const controls = modelControls();
      const control = controls[0];
      if (controls.length === 1 && !explicitEffortControl(control) && available(control) &&
          controlClosed(control) && !controlOpen(control) && isModel(text(control)) && normalize(text(control)) === target) return;
    }
    let menu = await open();
    if (window.__web2codexDeepProbe === false) {
      // Response-bound pages emit incremental evidence only. The native owner
      // merges these observations and does not treat them as a full snapshot.
      const current = menu.options.filter(selected);
      publish(labelsInMenu(menu), current.length === 1 && available(current[0]) && menu.effortVerified
        ? { [modelLabel(current[0])]: menu.efforts } : {});
    }
    const matchesTarget = option => normalize(modelLabel(option)) === target || normalize(text(option)) === target;
    let matches = menu.options.filter(matchesTarget);
    if (matches.length !== 1) throw new Error(`ChatGPT does not expose one exact picker choice for ${requested}`);
    if (!available(matches[0])) {
      throw new Error(`ChatGPT does not make model ${requested} available`);
    }
    if (selected(matches[0])) return;
    activate(matches[0]);
    await sleep(400);
    const control = findControl();
    if (control && !isEffort(text(control)) && normalize(text(control)) === target) return;
    menu = await open(menu.control);
    matches = menu.options.filter(matchesTarget);
    const confirmed = matches.length === 1 && selected(matches[0]);
    if (!confirmed) throw new Error(`ChatGPT did not confirm model ${requested}`);
  });
  const selectInstantForCompaction = withPickerCleanup(async requested => {
    const family = value => normalize(value)
      .replace(/[()[\]{}]/g, "")
      .replace(/(?:instant|thinking|pro|extended|standard|auto|default)$/, "");
    const targetFamily = family(requested);
    let menu = await open();
    const instantOptions = menu.options.filter(option => available(option) && /\binstant\b/i.test(modelLabel(option)));
    let matches = instantOptions.filter(option => family(modelLabel(option)) === targetFamily);
    if (!matches.length) {
      const broadFamily = value => family(value).replace(/(?:sol|astra)$/, "");
      matches = instantOptions.filter(option => broadFamily(modelLabel(option)) === broadFamily(requested));
    }
    if (matches.length !== 1) {
      throw new Error(`ChatGPT does not expose one available Instant model for ${requested}; compaction was not sent`);
    }
    if (selected(matches[0])) return modelLabel(matches[0]);
    activate(matches[0]);
    await sleep(400);
    menu = await open(menu.control);
    matches = menu.options.filter(option => available(option) && /\binstant\b/i.test(modelLabel(option)));
    let confirmedMatches = matches.filter(option => family(modelLabel(option)) === targetFamily);
    if (!confirmedMatches.length) {
      const broadFamily = value => family(value).replace(/(?:sol|astra)$/, "");
      confirmedMatches = matches.filter(option => broadFamily(modelLabel(option)) === broadFamily(requested));
    }
    if (confirmedMatches.length !== 1 || !selected(confirmedMatches[0])) {
      throw new Error(`ChatGPT did not confirm the Instant model for ${requested}; compaction was not sent`);
    }
    return modelLabel(confirmedMatches[0]);
  });
  window.__web2codexModelPicker = { select, selectInstantForCompaction, setEffort };
  // An inference view reads the menu as part of selecting the requested
  // model. A concurrent boot probe could close that menu during selection.
  if (window.__web2codexDeepProbe === false) return;
  window.__web2codexCapabilityProbeComplete = (async () => {
    let options = [];
    let reasoningByModel = {};
    let original = null;
    let snapshotComplete = false;
    try {
      // ChatGPT can render its controls before they are interactive. Retry
      // for up to a minute while the authenticated Temporary Chat hydrates.
      for (let attempt = 0; attempt < 30 && !original; attempt++) {
        try {
          const menu = await open(undefined, {
            controlTimeout: 500, surfaceAttempts: 3, surfaceTimeout: 250, modelOptionsTimeout: 250,
          });
          const candidateOptions = labelsInMenu(menu);
          const active = menu.options.filter(selected);
          if (candidateOptions.length && active.length === 1 && available(active[0])) {
            options = candidateOptions;
            original = modelLabel(active[0]);
          }
          await close(menu.control);
          if (!original) await sleep(750);
        } catch (error) {
          window.__web2codexCapabilityProbeError = String(error?.message || error);
          await close();
          await sleep(750);
        }
      }

      // The normal browser tab is authoritative: the native side replaces its
      // capability snapshot for this event. Do not publish the picker list as a
      // completed snapshot until every option was selected and inspected.
      if (!original || !options.length || !options.includes(original)) return;

      for (const name of options) {
        try {
          await select(name);
          const control = await waitFor(() => findEffortControl() || findControl(), 5000);
          if (!control) continue;
          const surface = await openEffortSurface(control, 5000);
          const efforts = readEffortCapabilities(surface?.row) ?? readEffortMenuCapabilities(surface?.control || control);
          if (efforts === null) continue;
          if (efforts.length) reasoningByModel[name] = efforts;
        } catch (_) {
          // One picker choice can lack a readable effort control. Keep the
          // independently verified choices and inspect the rest of the list.
        } finally {
          await close();
        }
      }

      await select(original);
      const restoredMenu = await open();
      const restoredSelection = restoredMenu.options.filter(selected);
      const restored = restoredSelection.length === 1 &&
        normalize(modelLabel(restoredSelection[0])) === normalize(original);
      await close(restoredMenu.control);
      if (!restored) throw new Error("ChatGPT did not restore the original model after its capability probe");

      snapshotComplete = true;
    } catch (error) {
      // An incomplete probe is not an authoritative capability snapshot. Keep
      // the previous native snapshot and let a later page/request probe retry.
      window.__web2codexCapabilityProbeError = String(error?.message || error);
    } finally {
      try { await close(); }
      catch (error) {
        snapshotComplete = false;
        window.__web2codexCapabilityProbeError = String(error?.message || error);
      }
      if (snapshotComplete) publish(options, reasoningByModel);
    }
  })();
})();
