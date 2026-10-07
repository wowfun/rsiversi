function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
export async function mount(root, initial, host, signal) {
  let previous;
  let body, currentBusy;
  let buttons = new Map();
  let events = new AbortController();
  signal.addEventListener("abort", () => events.abort(), { once: true });
  function update(snapshot) {
    const { model, busy, error } = snapshot;
    if (!model.standard_view) throw new Error("Standard renderer requires a validated view");
    const key = JSON.stringify(snapshot);
    if (key === previous) return;
    previous = key;
    currentBusy = busy;
    events.abort();
    events = new AbortController();
    const fields = new Map();
    body ??= element("div", "ui-contribution");
    const nodes = [], nextButtons = new Map();
    if (error) nodes.push(element("p", "source-error", error));
    for (const item of model.standard_view.elements) {
      if (item.kind === "text" || item.kind === "code") nodes.push(element(item.kind === "code" ? "pre" : "p", "ui-text", item.text));
      else if (item.kind === "field") {
        const row = element("p", "ui-field"); row.append(element("strong", "", `${item.label}: `), document.createTextNode(item.value)); nodes.push(row);
      } else if (item.kind === "input") {
        const label = element("label", "ui-input", item.label);
        const input = element(item.multiline ? "textarea" : "input");
        input.setAttribute("aria-label", item.label); input.dataset.uiField = item.name;
        input.value = host.input(item.name) ?? item.value; input.disabled = busy;
        input.addEventListener("input", () => host.setInput(item.name, input.value), { signal: events.signal });
        fields.set(item.name, input); label.append(input); nodes.push(label);
      } else if (item.kind === "button") {
        const key = JSON.stringify([item.action, item.label, item.value], (_key, value) =>
          value && typeof value === "object" && !Array.isArray(value)
            ? Object.fromEntries(Object.keys(value).sort().map(name => [name, value[name]])) : value);
        const action = buttons.get(key)?.shift() ?? element("button", "", item.label);
        const retained = nextButtons.get(key) ?? [];
        retained.push(action); nextButtons.set(key, retained);
        action.type = "button"; action.disabled = busy;
        action.addEventListener("click", async () => {
          action.disabled = true;
          try { await host.invoke(item.action, { value: item.value, fields: Object.fromEntries([...fields].map(([name, input]) => [name, input.value])) }); }
          catch (error) { if (!signal.aborted) body.prepend(element("p", "source-error", error.message)); }
          finally { if (!signal.aborted) action.disabled = currentBusy; }
        }, { signal: events.signal });
        nodes.push(action);
      }
    }
    buttons = nextButtons;
    if (busy) nodes.push(element("p", "hint", "Working…"));
    const focused = root.querySelector("[data-ui-field]:focus");
    const selection = focused && { key: focused.dataset.uiField, start: focused.selectionStart, end: focused.selectionEnd };
    const retained = new Set(nodes);
    for (const node of [...body.childNodes]) if (!retained.has(node)) node.remove();
    let next = body.firstChild;
    for (const node of nodes) {
      if (node === next) next = next.nextSibling;
      else body.insertBefore(node, next);
    }
    if (root.firstChild !== body) root.replaceChildren(body);
    if (selection) { const input = fields.get(selection.key); input?.focus(); input?.setSelectionRange(selection.start, selection.end); }
  }
  update(initial);
  return { async update(snapshot) { update(snapshot); }, async dispose() { events.abort(); buttons.clear(); root.replaceChildren(); } };
}
