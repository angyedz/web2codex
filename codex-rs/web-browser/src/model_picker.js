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
  const isEffort = value => /^(low|medium|high|низкий|средний|высокий)(?:\s|[›→>]|$)/i.test(value);
  const isModel = value => /^(?:chatgpt\s+)?gpt[\s\-‐‑‒–—−]*\d/i.test(value);
  const choices = () => [...document.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"], [data-model-id]')]
    .filter(visible).filter(element => isModel(text(element)))
    .filter(element => !element.parentElement?.closest('[role="menuitem"], [role="menuitemradio"], [role="option"]'));
  const waitFor = async (read, timeout) => {
    const deadline = Date.now() + timeout;
    do { const result = read(); if (result) return result; await sleep(100); } while (Date.now() < deadline);
    return null;
  };
  const findControl = () => [...document.querySelectorAll('button[data-testid="model-switcher-dropdown-button"], button[aria-label*="model" i], button[aria-label*="модел" i]')]
    .filter(visible).find(button => !button.closest('[role="menu"], [role="listbox"]'));
  const effortRow = control => {
    const anchor = control.getBoundingClientRect();
    return [...document.querySelectorAll('[role="menuitem"], [role="button"], button, [data-effort-only="true"]')]
      .filter(visible).filter(element => element !== control && !element.contains(control) && isEffort(text(element)))
      .filter(element => {
        const rect = element.getBoundingClientRect();
        return Math.abs(rect.x - anchor.x) < 250 && Math.abs(rect.y - anchor.y) < 200 && Math.abs(rect.y - anchor.y) > 10;
      }).sort((a, b) => a.getBoundingClientRect().width * a.getBoundingClientRect().height - b.getBoundingClientRect().width * b.getBoundingClientRect().height)[0];
  };
  const open = async () => {
    const control = await waitFor(findControl, 10000);
    if (!control) throw new Error("ChatGPT model picker is unavailable");
    control.click();
    let options = await waitFor(() => choices().length && choices(), 700);
    if (!options) {
      const row = await waitFor(() => effortRow(control), 3000);
      if (!row) throw new Error("ChatGPT model menu is unavailable");
      // The row contains a slider. Click its label, never the slider itself.
      const label = [...row.querySelectorAll("span, div")].filter(visible)
        .find(element => isEffort(text(element)) && !element.querySelector('[role="slider"], input[type="range"]'));
      activate(label || row);
      options = await waitFor(() => choices().length && choices(), 5000);
    }
    if (!options) throw new Error("ChatGPT did not show its model choices");
    return { control, options };
  };
  const close = () => { for (let i = 0; i < 4; i++) document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", bubbles: true })); };
  const selected = option => [option, ...option.querySelectorAll('[aria-checked], [aria-selected], [aria-pressed], [data-state]')]
    .some(node => node.getAttribute("aria-checked") === "true" || node.getAttribute("aria-selected") === "true" || node.getAttribute("aria-pressed") === "true" || node.getAttribute("data-state") === "checked");
  const select = async requested => {
    const target = normalize(requested);
    let menu = await open();
    let matches = menu.options.filter(option => normalize(text(option)) === target);
    if (matches.length !== 1) { close(); throw new Error(`ChatGPT does not expose one exact picker choice for ${requested}`); }
    if (selected(matches[0])) { close(); return; }
    activate(matches[0]);
    await sleep(400);
    const control = findControl();
    if (control && !isEffort(text(control)) && normalize(text(control)) === target) return;
    menu = await open();
    matches = menu.options.filter(option => normalize(text(option)) === target);
    const confirmed = matches.length === 1 && selected(matches[0]);
    close();
    if (!confirmed) throw new Error(`ChatGPT did not confirm model ${requested}`);
  };
  window.__web2codexModelPicker = { select };
  window.__web2codexCapabilityProbeComplete = (async () => {
    let options = [];
    try {
      // ChatGPT can render its controls before they are interactive. Retry
      // during hydration instead of delaying every turn by a fixed 12 seconds.
      for (let attempt = 0; attempt < 3 && options.length === 0; attempt++) {
        try {
          const menu = await open();
          options = [...new Set(menu.options.map(text).filter(value => value.length <= 120))].slice(0, 40);
        } catch (_) {
          close();
          await sleep(1000);
        }
      }
    } catch (_) {
      // An empty snapshot indicates that the live picker could not be read.
    } finally {
      close();
      window.ipc.postMessage(JSON.stringify({ type: "account-capabilities", probe_version: 3, picker_options: options }));
    }
  })();
})();
