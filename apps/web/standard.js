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
  let imageEpoch=0,imageUrl,pendingImage;
  const releaseImage=()=>{pendingImage=undefined;imageEpoch++;if(imageUrl)URL.revokeObjectURL(imageUrl);imageUrl=undefined;};
  signal.addEventListener("abort", () => events.abort(), { once: true });
  function update(snapshot) {
    const { model, busy, error } = snapshot;
    if (!model.standard_view) throw new Error("Standard renderer requires a validated view");
    const key = JSON.stringify(snapshot);
    if (key === previous) return;
    previous = key;
    currentBusy = busy;
    releaseImage();
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
    const image=model.data?.image;
    if(!busy&&image&&Number.isSafeInteger(image.bytes)&&image.bytes>0&&image.bytes<=4*1024*1024&&image.width===1280&&image.height===720&&model.sources?.some(source=>source.name===image.source&&source.media_type==="image/png")){
      const figure=element("figure","ui-screenshot"),img=element("img");img.alt="Current Session browser screenshot";img.width=1280;img.height=720;img.style.maxWidth="100%";img.style.height="auto";figure.append(img);nodes.splice(1,0,figure);
      const epoch=imageEpoch;
      pendingImage=async()=>{try{
        const data=new Uint8Array(image.bytes);let offset=0;
        while(offset<data.length){if(signal.aborted||epoch!==imageEpoch)return;const chunk=await host.source(image.source,offset,Math.min(65536,data.length-offset));if(!chunk.length||chunk.length>data.length-offset)throw new Error("Incomplete screenshot source");data.set(chunk,offset);offset+=chunk.length;}
        if(signal.aborted||epoch!==imageEpoch)return;
        if(data[0]!==137||data[1]!==80||data[2]!==78||data[3]!==71)throw new Error("Invalid screenshot source");
        imageUrl=URL.createObjectURL(new Blob([data],{type:"image/png"}));img.src=imageUrl;
      }catch(error){if(!signal.aborted&&epoch===imageEpoch)figure.replaceChildren(element("p","source-error",error.message));}};
    }
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
  return { async update(snapshot) { update(snapshot); }, activate() { const load=pendingImage;pendingImage=undefined;void load?.(); }, async dispose() { events.abort(); releaseImage(); buttons.clear(); root.replaceChildren(); } };
}
