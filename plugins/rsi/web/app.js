const $ = id => document.getElementById(id);
const pending = new Map();
let worker;
let requestId = 0;
let view;
let selected = 0;
let connected = false;
let catalogKey;
let dialogKey;
let lastNotice;
const imageCache = new Map();
let imageEpoch = 0;
let imageReading = false;
function clearImages() {
  imageEpoch++;
  for (const entry of imageCache.values()) URL.revokeObjectURL(entry.url);
  imageCache.clear();
}
async function previewImage(body, media, ticket) {
  const key = JSON.stringify(media);
  const epoch = imageEpoch;
  const status = element("p", "hint", "Loading image…");
  body.append(status);
  try {
    let entry = imageCache.get(key);
    if (!entry) {
      if (imageReading) throw new Error("An image preview is still loading; reopen this preview to retry");
      imageReading = true;
      let bytes;
      try { bytes = await call("read_image", JSON.stringify({ kind: "source", ticket })); }
      finally { imageReading = false; }
      if (epoch !== imageEpoch || !body.isConnected) return;
      const limits = view.media_limits;
      if (bytes.byteLength !== media.bytes || bytes.byteLength > limits.preview_bytes) throw new Error("Image exceeds the preview budget");
      while (imageCache.size >= limits.preview_objects || [...imageCache.values()].reduce((sum, entry) => sum + entry.bytes, 0) + bytes.byteLength > limits.preview_bytes) {
        const oldest = imageCache.keys().next().value;
        URL.revokeObjectURL(imageCache.get(oldest).url); imageCache.delete(oldest);
      }
      entry = { url: URL.createObjectURL(new Blob([bytes], { type: "image/png" })), bytes: bytes.byteLength };
    }
    if (epoch !== imageEpoch || !body.isConnected) return;
    imageCache.delete(key); imageCache.set(key, entry);
    const img = element("img", "image-preview"); img.alt = `Image ${media.width} × ${media.height}`;
    img.src = entry.url;
    img.onerror = () => { if (body.isConnected) status.textContent = "Image could not be displayed"; };
    img.onload = () => { if (body.isConnected) status.remove(); };
    body.append(img);
  } catch (error) { if (epoch === imageEpoch && body.isConnected) status.textContent = `Image unavailable: ${error.message}`; }
}
let endpoint;
try { endpoint = localStorage.getItem("rsi.endpoint"); } catch { /* Storage is optional. */ }

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function markdown(nodes) {
  const fragment = document.createDocumentFragment();
  const stack = [fragment];
  for (const node of nodes) {
    const parent = stack.at(-1);
    if (node.kind === "start") {
      const value = node.element;
      const tags = { paragraph: "p", quote: "blockquote", pre: "pre", item: "li", emphasis: "em", strong: "strong", strike: "s", span: "span", link: "a" };
      const tag = value.kind === "heading" ? ["h1", "h2", "h3", "h4", "h5", "h6"][value.level - 1] : value.kind === "list" ? (value.start === null ? "ul" : "ol") : tags[value.kind];
      const child = element(tag ?? "span");
      if (value.kind === "link") { child.href = value.href; child.target = "_blank"; child.rel = "noopener noreferrer"; }
      if (value.kind === "list" && value.start !== null) child.start = value.start;
      parent.append(child); stack.push(child);
    } else if (node.kind === "end") { stack.pop(); }
    else if (node.kind === "text") parent.append(document.createTextNode(node.text));
    else if (node.kind === "code") parent.append(element("code", undefined, node.text));
    else if (node.kind === "break") parent.append(element("br"));
    else if (node.kind === "rule") parent.append(element("hr"));
  }
  return fragment;
}
function button(label, run, className) {
  const node = element("button", className, label);
  node.type = "button";
  node.addEventListener("click", () => perform(run));
  return node;
}
function notify(message) { $("notice").textContent = message; $("notice").hidden = !message; }
async function perform(run) {
  try { await run(); } catch (error) { notify(String(error.message ?? error)); }
}
function failWorker(error) {
  clearImages();
  connected = false;
  for (const waiter of pending.values()) waiter.reject(new Error(error));
  pending.clear();
  worker?.terminate(); worker = undefined;
  $("connection-state").textContent = "Connection failed";
  $("connection-state").classList.remove("connected");
  $("login").hidden = false;
  $("workbench").hidden = true;
  $("sign-out").hidden = true;
  notify(`${error}. Reconnect explicitly to start a new connection.`);
}
let frameId;
function presentFrame(frame) {
  if (typeof frame.frame_id !== "string" || !/^[1-9][0-9]{0,19}$/.test(frame.frame_id) || BigInt(frame.frame_id) > 18446744073709551615n) throw new Error("Invalid presentation frame ID");
  let next;
  if (frame.kind === "snapshot") {
    next = frame.view;
  } else if (frame.kind === "patch") {
    if (!view || frame.base_frame_id !== frameId) return false;
    if (BigInt(frame.frame_id) !== BigInt(frameId) + 1n) return false;
    next = { ...view, ...frame.sections, panes: [...view.panes] };
    for (const change of frame.panes) {
      if (![0, 1].includes(change.index) || !next.panes[change.index]) throw new Error("Invalid pane patch");
      const pane = { ...next.panes[change.index], ...change.fields };
      if (change.transcript) {
        const delta = change.transcript;
        const blocks = new Map(pane.transcript.blocks.map(block => [block.key, block]));
        for (const key of delta.remove) blocks.delete(key);
        for (const block of delta.upsert) blocks.set(block.key, block);
        const order = delta.order ?? pane.transcript.blocks.map(block => block.key);
        if (new Set(order).size !== order.length || order.length !== blocks.size || order.some(key => !blocks.has(key))) throw new Error("Invalid block order");
        pane.transcript = { ...pane.transcript, ...delta.fields, blocks: order.map(key => blocks.get(key)) };
      }
      next.panes[change.index] = pane;
    }
  } else { throw new Error("Unknown presentation frame"); }
  if (!Array.isArray(next?.panes) || next.panes.length !== 2) throw new Error("Invalid presentation snapshot");
  render(next);
  frameId = frame.frame_id;
  return true;
}
function makeWorker() {
  frameId = undefined;
  const current = new Worker("/worker.js", { type: "module" });
  current.onmessage = ({ data }) => {
    if (worker !== current) return;
    if (data.kind === "view") {
      try {
        const frame = JSON.parse(data.view);
        const accepted = presentFrame(frame);
        if (worker === current) current.postMessage({ kind: "ack", frame_id: frame.frame_id, resync: !accepted });
      } catch (error) { failWorker(`View rendering failed: ${error.message}`); }
    } else if (data.kind === "reply") {
      const waiter = pending.get(data.id); pending.delete(data.id);
      if (data.error) waiter?.reject(new Error(data.error)); else waiter?.resolve(data.result);
    } else if (data.kind === "failed") { failWorker(data.error); }
  };
  current.onerror = event => { event.preventDefault(); if (worker === current) failWorker("Browser Worker stopped"); };
  return current;
}
function call(method, payload, transfer = []) {
  if (!worker) return Promise.reject(new Error("Connect to your service first"));
  if (pending.size >= 8) return Promise.reject(new Error("Input is busy; wait for the current action"));
  const id = ++requestId;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ kind: "call", id, method, payload }, transfer);
  });
}
function command(value) { return call("command", JSON.stringify(value)); }

async function connectWith(receipt) {
  $("connect").disabled = true; $("reconnect").disabled = true;
  notify(""); $("connection-state").textContent = "Connecting…";
  if (!worker) worker = makeWorker();
  try {
    endpoint = await call("connect", { receipt, devHttp: $("dev-http").checked });
    try { localStorage.setItem("rsi.endpoint", endpoint); } catch { /* Storage is optional. */ }
    connected = true;
    $("connection-state").textContent = "Connected";
    $("connection-state").classList.add("connected");
    $("login").hidden = true; $("workbench").hidden = false; $("sign-out").hidden = false;
    $("reconnect").hidden = false;
  } catch (error) { failWorker(error.message); }
  finally { $("connect").disabled = false; $("reconnect").disabled = false; }
}
$("login-form").addEventListener("submit", event => {
  event.preventDefault();
  const receipt = $("receipt").value;
  $("receipt").value = "";
  if (receipt.length > 2048) { notify("Device receipt exceeds its limit"); return; }
  perform(() => connectWith(receipt));
});
$("dev-http-label").hidden = location.protocol !== "http:";
$("reconnect").hidden = !endpoint;
$("reconnect").addEventListener("click", () => perform(() => connectWith(JSON.stringify({ endpoint_id: endpoint }))));
$("sign-out").addEventListener("click", () => perform(async () => {
  $("sign-out").disabled = true;
  try {
    await Promise.all(panes.map(pane => pane.flush()));
    let resources;
    try { resources = await call("disconnect", true); }
    catch (error) { failWorker(String(error.message ?? error)); return; }
    document.dispatchEvent(new CustomEvent("rsi-disconnected", { detail: resources }));
    connected = false;
    worker.terminate(); worker = undefined;
    view = undefined; catalogKey = undefined; dialogKey = undefined; lastNotice = undefined;
    clearImages();
    for (const pane of panes) pane.reset();
    $("connection-state").textContent = "Disconnected";
    $("connection-state").classList.remove("connected");
    $("workbench").hidden = true; $("login").hidden = false; $("sign-out").hidden = true;
    $("detail").close(); notify("");
  } finally { $("sign-out").disabled = false; }
}));

class Pane {
  constructor(index) {
    this.index = index;
    this.blocks = new Map();
    this.generation = undefined;
    this.unsent = false;
    this.draftWork = undefined;
    this.draftError = undefined;
    this.enterSubmit = false;
    this.images = [];
    this.imageEdits = 0;
    this.node = element("section", "pane");
    this.node.setAttribute("aria-label", `${index ? "Right" : "Left"} conversation`);
    this.node.addEventListener("focusin", () => select(index));
    const header = element("header", "pane-header");
    const heading = element("div", "pane-heading");
    this.name = element("span", "pane-name", "New conversation");
    this.status = element("span", "pane-status", "Ready");
    heading.append(this.name, this.status);
    this.session = element("p", "pane-session mono", "Select a workspace to begin");
    header.append(heading, this.session);
    const tools = element("div", "pane-tools");
    this.history = button("Earlier history", () => this.action("history"), "quiet");
    this.live = button("Back to live", () => this.action("live"), "quiet");
    this.commands = button("Session commands", () => this.action("commands"), "quiet");
    this.uiMenu = element("span", "ui-menu");
    tools.append(this.history, this.live, this.commands, this.uiMenu);
    this.commandView = element("div", "session-commands");
    this.extensionView = element("details", "session-extensions");
    this.extensionView.setAttribute("aria-label", "Extension state");
    this.transcript = element("div", "transcript");
    this.transcript.setAttribute("aria-label", "Conversation transcript");
    this.transcript.tabIndex = 0;
    this.waiting = element("div", "pending");
    this.notice = element("div", "pane-notice");
    this.composer = element("form", "composer");
    this.input = element("textarea");
    this.input.setAttribute("aria-label", `${index ? "Right" : "Left"} message`);
    this.input.placeholder = "Describe the work…";
    this.input.maxLength = 1024 * 1024;
    this.input.addEventListener("input", () => {
      this.unsent = true; this.draftError = undefined;
      this.pump();
    });
    this.input.addEventListener("keydown", event => {
      if (event.isComposing || event.keyCode === 229) return;
      if (event.key === "Enter" && !event.shiftKey && (event.metaKey || event.ctrlKey || this.enterSubmit)) { event.preventDefault(); perform(() => this.submit(false)); }
    });
    this.composer.addEventListener("submit", event => { event.preventDefault(); perform(() => this.submit(false)); });
    const bar = element("div", "composer-bar");
    this.model = element("select"); this.model.setAttribute("aria-label", `${index ? "Right" : "Left"} model`);
    this.model.addEventListener("change", () => perform(() => this.action("model", { model: JSON.parse(this.model.value) })));
    const actions = element("div", "actions");
    this.imageInput = element("input"); this.imageInput.type = "file"; this.imageInput.accept = "image/*"; this.imageInput.multiple = true;
    this.imageInput.hidden = true; this.imageInput.setAttribute("aria-label", `${index ? "Right" : "Left"} image files`);
    this.imageInput.addEventListener("change", () => {
      const files = [...this.imageInput.files]; this.imageInput.value = "";
      perform(() => this.upload(files));
    });
    this.attach = button("Add images", () => this.imageInput.click(), "quiet");
    this.imageList = element("div", "draft-images");
    this.frozenImages = element("p", "hint frozen-images");
    this.cancel = button("Cancel", () => this.action("cancel"), "quiet");
    this.steer = button("Steer", () => this.submit(true));
    this.send = button("Send ↗", () => this.submit(false), "primary");
    actions.append(this.attach, this.cancel, this.steer, this.send); bar.append(this.model, actions);
    this.hint = element("div", "composer-hint", "Ctrl / ⌘ Enter to send · Enter for a new line");
    this.composer.append(this.input, this.imageInput, this.imageList, this.frozenImages, bar, this.hint);
    this.node.append(header, tools, this.commandView, this.extensionView, this.transcript, this.waiting, this.notice, this.composer);
    $("panes").append(this.node);
    this.render(null, []);
  }
  action(action, fields = {}) {
    if (!this.generation) throw new Error("Open a conversation first");
    return command({ action, pane: this.index, generation: this.generation, ...fields });
  }
  pump() {
    if (this.draftWork || !this.unsent || !this.generation) return;
    const generation = this.generation;
    this.draftWork = (async () => {
      while (this.unsent && generation === this.generation) {
        const text = this.input.value;
        this.unsent = false;
        try { await this.action("draft", { text }); }
        catch (error) { this.unsent = true; this.draftError = error; notify(error.message); break; }
      }
    })().finally(() => { this.draftWork = undefined; });
  }
  async flush() {
    this.pump(); await this.draftWork;
    if (this.unsent) throw this.draftError ?? new Error("Draft has not reached the application");
  }
  async submit(steer) {
    if (this.submitting || this.uploading) return;
    this.submitting = true; this.send.disabled = true; this.steer.disabled = true;
    try {
      await this.flush();
      const generation = this.generation;
      const text = this.input.value;
      const submittedText = this.retryText ?? text;
      const images = JSON.stringify(this.images);
      const imageEdits = this.imageEdits;
      const submittedImages = JSON.stringify(this.retryImages ?? this.images);
      await this.action("submit", { text, steer });
      if (this.generation === generation && this.input.value === text && text === submittedText && images === submittedImages && imageEdits === this.imageEdits) this.input.value = "";
    } finally { this.submitting = false; this.send.disabled = !this.generation; this.steer.disabled = !this.generation || this.retryText != null; }
  }
  async upload(files) {
    if (!files.length) return;
    if (this.uploading || !this.generation) throw new Error("Image import is unavailable while this pane is busy");
    const limits = view.media_limits;
    if (this.images.length + files.length > limits.images) throw new Error(`A draft can hold at most ${limits.images} images`);
    if (files.some(file => !file.size || file.size > limits.upload_bytes)) throw new Error("Each image source must contain 1 byte to 16 MiB");
    const generation = this.generation;
    this.imageEdits++;
    this.uploading = true; this.attach.disabled = true; this.send.disabled = true; this.steer.disabled = true;
    try {
      await this.flush();
      for (const file of files) {
        const bytes = await file.arrayBuffer();
        if (this.generation !== generation) throw new Error("Pane changed; remaining images were not imported");
        await call("import_image", { pane: this.index, generation, bytes }, [bytes]);
      }
    } finally {
      this.uploading = false;
      this.render(view?.panes[this.index], view?.catalog.models ?? []);
    }
  }
  renderImages(data) {
    this.images = data?.images ?? [];
    this.retryImages = data?.unresolved_images;
    this.attach.hidden = !view?.has_media;
    this.attach.disabled = !data || this.uploading || this.submitting || this.switching;
    this.frozenImages.textContent = this.retryImages?.length ? `Previous submission retains ${this.retryImages.length} image(s) in its original order. Draft changes apply to the next submission.` : "";
    const key = JSON.stringify([data?.generation, data?.images_revision, this.images]);
    if (key === this.imagesKey) return;
    this.imagesKey = key;
    this.imageList.replaceChildren(...this.images.map((media, index) => {
      const row = element("div", "draft-image");
      row.append(element("span", "", `${index + 1}. ${media.width} × ${media.height} · ${media.bytes} bytes`));
      const edit = to => { this.imageEdits++; return this.action("image_edit", { revision: data.images_revision, from: index, to }); };
      const earlier = button("Move image earlier", () => edit(index - 1), "quiet"); earlier.disabled = index === 0;
      const later = button("Move image later", () => edit(index + 1), "quiet"); later.disabled = index + 1 === this.images.length;
      row.append(button("Preview image", () => this.action("inspect_image", { index, media }), "quiet"), earlier, later, button("Remove image", () => edit(null), "quiet"));
      return row;
    }));
  }
  reset() { this.generation = undefined; this.unsent = false; this.draftError = undefined; this.input.value = ""; this.render(null, []); }
  renderCommands(data) {
    this.commands.disabled = !data || this.switching;
    const key = JSON.stringify([data?.generation, data?.commands, data?.command_submission]);
    if (key === this.commandKey) return;
    this.commandKey = key;
    this.commandView.replaceChildren();
    for (const item of data?.commands?.commands ?? []) {
      const select = button(`/${item.name}`, () => {
        this.input.value = `/${item.name} `;
        this.unsent = true; this.draftError = undefined; this.pump(); this.input.focus();
      }, "quiet");
      select.title = item.description;
      select.disabled = data.commands.revision.kind === "draft" && !item.draft_safe;
      this.commandView.append(select, element("span", "command-description", item.description));
    }
    const state = data?.command_submission;
    if (state?.pending) {
      this.commandView.append(element("p", "", `Command result unresolved · ${state.pending.request_id}`),
        element("pre", "command-input", JSON.stringify(state.pending, null, 2)),
        button("Refresh command result", () => this.action("refresh_command_result"), "quiet"));
    } else if (state?.receipt) {
      const result = state.receipt.outcome;
      this.commandView.append(element("p", "command-receipt", `${state.receipt.command} · ${result.kind === "draft_changed" ? `Draft changed · revision ${result.revision}` : `Committed · control ${result.control_seq}`} · ${state.receipt.request_id}`));
    }
    this.commandView.hidden = this.commandView.childElementCount === 0;
  }
  renderExtensions(data) {
    const key = JSON.stringify([data?.generation, data?.projections, data?.projection_notice]);
    if (key === this.extensionKey) return;
    this.extensionKey = key;
    this.extensionView.replaceChildren();
    this.extensionView.hidden = !data;
    const snapshot = data?.projections;
    const status = data?.projection_notice ? "Unavailable · last snapshot" : snapshot ? (snapshot.cursor.kind === "draft" ? `Draft · revision ${snapshot.cursor.revision}` : `Durable · Fact ${snapshot.cursor.fact_seq} · control ${snapshot.cursor.control_seq}`) : "Loading";
    this.extensionView.append(element("summary", "", `Extension state · ${status}`));
    if (data?.projection_notice) this.extensionView.append(element("p", "extension-error", data.projection_notice));
    for (const entry of snapshot?.entries ?? []) {
      const section = element("section", "extension-value");
      section.dataset.producer = entry.producer;
      section.append(element("strong", "", entry.producer), element("pre", entry.content.kind === "failed" ? "extension-error" : "", entry.content.kind === "failed" ? `Producer failed: ${entry.content.message}` : JSON.stringify(entry.content.value, null, 2)));
      this.extensionView.append(section);
    }
    if (snapshot && !snapshot.entries.length) this.extensionView.append(element("p", "", "No extension views in this preset"));
  }
  render(data, models) {
    const changed = this.generation !== data?.generation;
    if (changed) {
      this.switching = false;
      this.generation = data?.generation;
      this.unsent = false; this.draftError = undefined;
      this.input.value = data?.draft ?? "";
      this.blocks.clear(); this.transcript.replaceChildren();
      this.pendingKey = undefined;
    }
    this.uiCards = !!data?.ui_cards;
    const uiKey = JSON.stringify([data?.generation, data?.ui_surfaces]);
    if (uiKey !== this.uiKey) {
      this.uiKey = uiKey;
      this.uiMenu.replaceChildren(...(data?.ui_surfaces ?? []).map(surface => button(surface.title,
        () => this.action("ui_surface", { reference: surface.reference }), "quiet")));
    }
    this.name.textContent = data ? basename(data.path) : "New conversation";
    this.session.textContent = data ? `${data.path} · ${data.session}` : "Select a workspace to begin";
    this.session.title = this.session.textContent;
    this.status.textContent = data?.historical ? "History" : (data?.transcript.status || "Ready");
    this.history.disabled = !data || (!data.history_more && data.historical);
    this.live.hidden = !data?.historical;
    this.input.disabled = !data || this.switching; this.model.disabled = !data || this.switching;
    this.send.disabled = !data || this.submitting || this.uploading || this.switching; this.steer.disabled = !data || this.submitting || this.uploading || this.switching;
    this.retryText = data?.unresolved_text;
    this.send.textContent = this.retryText != null ? "Retry previous" : "Send ↗";
    this.steer.disabled ||= this.retryText != null;
    this.cancel.disabled = !data;
    this.renderCommands(data);
    this.renderImages(data);
    this.renderExtensions(data);
    if (!data) {
      if (!this.transcript.querySelector(".empty-pane")) {
        const empty = element("div", "empty-pane");
        const glyph = element("div", "empty-glyph"); glyph.setAttribute("aria-hidden", "true"); glyph.append(element("span"), element("span"));
        empty.append(glyph, element("h3", "", this.index ? "A second line of thought." : "Make room for the work."),
          element("p", "", "Choose a workspace or reopen a conversation from the sidebar."));
        this.transcript.replaceChildren(empty);
      }
      this.waiting.replaceChildren(); this.pendingKey = undefined; this.notice.textContent = ""; return;
    }
    if (!this.unsent && !this.draftWork && !this.submitting && document.activeElement !== this.input && this.input.value !== data.draft) this.input.value = data.draft;
    const allModels = models.some(model => sameModel(model, data.model)) ? models : [data.model, ...models];
    const modelKey = JSON.stringify(allModels);
    if (modelKey !== this.modelKey) {
      this.modelKey = modelKey;
      this.model.replaceChildren(...allModels.map(model => {
        const option = element("option", "", `${model.model} · ${model.deployment}`); option.value = JSON.stringify(model); return option;
      }));
    }
    this.model.value = JSON.stringify(data.model);
    this.renderTranscript(data.transcript, changed);
    const pendingKey = JSON.stringify(data.pending);
    if (pendingKey !== this.pendingKey) {
      this.pendingKey = pendingKey;
      this.waiting.replaceChildren(...data.pending.map(item => button(`${item.kind === "approval" ? "Review" : "Answer"}: ${item.title}`,
        () => this.action("inspect_interaction", { owner: item.owner, id: item.id }))));
    }
    this.notice.textContent = data.notice;
  }
  renderTranscript(transcript, changed) {
    const atEnd = this.transcript.scrollHeight - this.transcript.clientHeight - this.transcript.scrollTop < 70;
    const keys = new Set(transcript.blocks.map(block => block.key));
    for (const [key, entry] of this.blocks) if (!keys.has(key)) { entry.node.remove(); this.blocks.delete(key); }
    if (this.transcript.querySelector(".empty-pane")) this.transcript.replaceChildren();
    let previous;
    for (const block of transcript.blocks) {
      let entry = this.blocks.get(block.key);
      if (!entry) {
        const node = element("article", `message ${block.role}`);
        const title = element("p", "message-title"); const text = element("div", "message-text"); const clipped = element("p", "omitted", "Text shortened in this view.");
        const sources = element("div", "actions source-actions");
        node.append(title, text, clipped, sources); entry = { node, title, text, clipped, sources }; this.blocks.set(block.key, entry);
      }
      if (entry.title.textContent !== block.title) entry.title.textContent = block.title;
      if (entry.source !== block.text || entry.markdown !== Boolean(block.markdown)) {
        entry.source = block.text; entry.markdown = Boolean(block.markdown);
        entry.text.classList.toggle("markdown", entry.markdown);
        if (block.markdown) entry.text.replaceChildren(markdown(block.markdown));
        else entry.text.textContent = block.text;
      }
      entry.clipped.hidden = !block.clipped;
      const sourceKey = JSON.stringify([block.tool, block.sources, this.uiCards]);
      if (sourceKey !== entry.sourceKey) {
        entry.sourceKey = sourceKey;
        entry.sources.replaceChildren();
        if (this.uiCards) entry.sources.append(button("Card details", () => this.action("ui_block", { key: block.key }), "quiet"));
        if (block.sources > 0) entry.sources.append(button("Inspect sources", () => this.action("inspect_block", { key: block.key }), "quiet"));
        if (block.tool) {
          for (const [field, label] of [["arguments", "Inspect arguments"], ["result", "Inspect result"], ["rejection", "Inspect rejection"]]) {
            const source = block.tool[field];
            if (source) entry.sources.append(button(label, () => this.action("inspect_source", { source }), "quiet"));
          }
          if (!block.tool.intent_present && block.tool.phase !== "rejected") entry.sources.append(element("span", "hint", "Intent not loaded"));
        }
      }
      const expected = previous ? previous.nextSibling : this.transcript.firstChild;
      if (expected !== entry.node) this.transcript.insertBefore(entry.node, expected);
      previous = entry.node;
    }
    let omitted = this.transcript.querySelector(":scope > .omitted");
    if (transcript.omitted) {
      omitted ??= element("p", "omitted", "Older content was omitted from this view. Open history to read earlier facts.");
      this.transcript.prepend(omitted);
    }
    if (!transcript.omitted) omitted?.remove();
    if (changed || atEnd) this.transcript.scrollTop = this.transcript.scrollHeight;
  }
}
function basename(path) { return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path; }
function sameModel(left, right) { return left.deployment === right.deployment && left.model === right.model; }
const panes = [new Pane(0), new Pane(1)];
function select(index) {
  selected = index;
  panes.forEach((pane, i) => { pane.node.classList.toggle("selected", index === i); $(`pane-tab-${i}`).setAttribute("aria-pressed", String(index === i)); });
}
select(0);
for (let i = 0; i < 2; i++) $(`pane-tab-${i}`).addEventListener("click", () => select(i));
async function openInSelected(fields) {
  const pane = panes[selected];
  if (pane.switching) throw new Error("This pane is still opening a conversation");
  pane.switching = true;
  pane.input.disabled = true; pane.model.disabled = true; pane.send.disabled = true; pane.steer.disabled = true;
  try {
    await pane.flush();
    await command({ ...fields, pane: pane.index });
  } catch (error) {
    pane.switching = false;
    pane.render(view?.panes[pane.index], view?.catalog.models ?? []);
    throw error;
  }
}
function render(next) {
  view = next;
  if (next.notice !== lastNotice) { lastNotice = next.notice; notify(next.notice); }
  const key = JSON.stringify(next.catalog);
  if (key !== catalogKey) {
    catalogKey = key;
    renderNavigation(next.catalog);
  }
  next.panes.forEach((data, i) => {
    panes[i].enterSubmit = next.preferences?.enter_submit ?? false;
    panes[i].hint.textContent = panes[i].enterSubmit ? "Enter to send · Shift Enter for a new line" : "Ctrl / ⌘ Enter to send · Enter for a new line";
    panes[i].render(data, next.catalog.models);
  });
  renderDetail(next);
}
function navItem(name, subtitle, run) {
  const item = button("", run, "nav-item"); item.title = subtitle;
  item.append(element("strong", "", name), element("small", "mono", subtitle)); return item;
}
function renderNavigation(catalog) {
  $("workspaces").replaceChildren(...catalog.workspaces.map(item => navItem(basename(item.path), item.path,
    () => openInSelected({ action: "create", workspace: item.id, trust: $("workspace-trust").checked }))));
  if (!catalog.workspaces.length) $("workspaces").append(element("p", "hint", "Add a directory on your service to start."));
  $("sessions").replaceChildren(...catalog.sessions.map(item => navItem(basename(item.path), item.id,
    () => openInSelected({ action: "open", session: item.id }))));
  if (!catalog.sessions.length) $("sessions").append(element("p", "hint", "Send a message, then refresh to list the conversation."));
  $("workspaces-next").hidden = !catalog.workspaces_more; $("sessions-next").hidden = !catalog.sessions_more;
  $("models-next").hidden = !catalog.models_more;
}
$("refresh").addEventListener("click", () => perform(() => command({ action: "refresh" })));
$("workspaces-next").addEventListener("click", () => perform(() => command({ action: "workspaces_next" })));
$("sessions-next").addEventListener("click", () => perform(() => command({ action: "sessions_next" })));
$("models-next").addEventListener("click", () => perform(() => command({ action: "models_next" })));
$("workspace-form").addEventListener("submit", event => {
  event.preventDefault(); perform(async () => { await command({ action: "register_workspace", path: $("workspace-path").value }); $("workspace-path").value = ""; });
});

function showDialog(key, title, body) {
  if (dialogKey === key) return;
  dialogKey = key; $("detail-title").textContent = title; $("detail-body").replaceChildren(body);
  if (!$("detail").open) $("detail").showModal();
}
async function closeDetail() { await command({ action: "close_detail" }); dialogKey = undefined; $("detail").close(); }
$("detail-close").addEventListener("click", () => perform(closeDetail));
$("detail").addEventListener("cancel", event => { event.preventDefault(); perform(closeDetail); });
$("settings-open").addEventListener("click", () => perform(() => command({ action: "settings_list" })));
function renderDetail(next) {
  if (next.image_detail) {
    const detail = next.image_detail;
    const key = `image:${detail.ticket}`;
    if (dialogKey === key) return;
    const body = element("div", "image-detail");
    body.append(element("p", "hint", `${detail.media.width} × ${detail.media.height} · ${detail.media.bytes} bytes`));
    showDialog(key, "Image preview", body);
    previewImage(body, detail.media, detail.ticket); return;
  }
  if (next.ui_detail) { renderUiDetail(next.ui_detail); return; }
  if (next.settings_catalog) {
    const catalog = next.settings_catalog;
    const key = JSON.stringify(catalog);
    if (dialogKey === key) return;
    const body = element("div", "settings-catalog");
    if (catalog.error) body.append(element("p", "settings-error", catalog.error));
    else if (!catalog.page) body.append(element("p", "", catalog.namespace ? `Reading ${catalog.namespace}…` : "Loading registered settings…"));
    else {
      const list = element("div", "settings-list");
      for (const namespace of catalog.page.namespaces) list.append(button(namespace, () => command({ action: "settings_read", namespace }), "settings-namespace quiet"));
      if (!catalog.page.namespaces.length) list.append(element("p", "", "No registered settings."));
      const actions = element("div", "actions");
      const nextPage = button("More settings", () => command({ action: "settings_next", ticket: catalog.ticket }));
      nextPage.disabled = catalog.page.next == null;
      actions.append(button("Refresh settings", () => command({ action: "settings_list" })), nextPage);
      body.append(list, actions);
    }
    showDialog(key, "Settings", body); return;
  }
  if (next.block_sources) {
    const detail = next.block_sources;
    const key = `block-sources:${detail.ticket}`;
    if (dialogKey === key) return;
    const body = element("div", "block-sources");
    body.append(element("p", "hint", `Sources ${detail.start + (detail.page.length ? 1 : 0)}–${detail.start + detail.page.length} of ${detail.total}`));
    const list = element("div", "source-list");
    for (const source of detail.page) {
      list.append(button(`Fact ${source.seq} · ${source.field.kind}${source.field.index == null ? "" : ` ${source.field.index}`}`,
        () => command({ action: "inspect_source", pane: detail.pane, generation: detail.generation, source }), "source-reference quiet"));
    }
    const actions = element("div", "actions");
    const previous = button("Previous sources", () => command({ action: "block_sources_page", ticket: detail.ticket, forward: false }));
    const nextPage = button("Next sources", () => command({ action: "block_sources_page", ticket: detail.ticket, forward: true }));
    previous.disabled = detail.start === 0; nextPage.disabled = detail.start + detail.page.length >= detail.total;
    actions.append(previous, nextPage); body.append(list, actions);
    showDialog(key, "Block sources", body); return;
  }
  if (next.source_detail) {
    const detail = next.source_detail;
    const key = JSON.stringify(detail);
    if (dialogKey === key) return;
    const body = element("div", "source-detail");
    body.append(element("p", "hint", `Fact ${detail.source.seq} · ${detail.source.field.kind}`));
    if (detail.error) body.append(element("p", "source-error", `Source unavailable: ${detail.error}`));
    else if (!detail.window) body.append(element("p", "", "Loading source…"));
    else {
      const window = detail.window;
      body.append(element("p", "source-range", `Bytes ${window.start}–${window.end}${window.more ? " · more available" : " · end"}`), element("pre", "source-text", window.text));
      const actions = element("div", "actions");
      const previous = button("Previous source page", () => command({ action: "source_page", ticket: detail.ticket, forward: false }));
      const nextPage = button("Next source page", () => command({ action: "source_page", ticket: detail.ticket, forward: true }));
      previous.disabled = window.start === 0; nextPage.disabled = !window.more;
      actions.append(previous, nextPage); body.append(actions);
      if (next.source_media && next.has_media) {
        const preview = element("div", "image-detail");
        const open = button("Preview source image", async () => {
          open.disabled = true; preview.replaceChildren();
          await previewImage(preview, next.source_media, detail.ticket);
          open.disabled = false;
        }, "quiet");
        body.append(open, preview);
      }
    }
    showDialog(key, "Exact source", body); return;
  }
  if (next.settings) {
    const editor = next.settings;
    if (dialogKey === editor.ticket) return;
    const form = element("form"); const text = element("textarea", "settings-text"); text.value = editor.text; text.spellcheck = false; text.setAttribute("aria-label", "Settings JSON");
    const save = element("button", "primary", "Save settings"); save.type = "submit";
    const description = editor.description;
    const applies = { live: "Applies live", new_session: "Applies to new conversations", restart: "Restart required" }[description.metadata.applies];
    form.append(element("p", "settings-applies", applies), element("p", "hint", description.metadata.description));
    for (const [label, value] of [["Schema", description.metadata.schema], ["Defaults", description.defaults]]) {
      const disclosure = element("details", "settings-description");
      disclosure.append(element("summary", "", label), element("pre", "", JSON.stringify(value, null, 2))); form.append(disclosure);
    }
    if (description.metadata.sensitive_fields.length) form.append(element("p", "hint", `Sensitive fields: ${description.metadata.sensitive_fields.map(path => path.join(" / ") || "(root)").join(", ")}`));
    form.append(element("p", "hint", "Saving requires the version you opened."), text, save);
    if (!description.writable) { text.readOnly = true; save.disabled = true; form.append(element("p", "hint", "Settings provider is read-only.")); }
    if (new TextEncoder().encode(editor.text).length > 1024 * 1024) {
      text.readOnly = true; save.disabled = true; form.append(element("p", "hint", "This value exceeds the Web editor's 1 MiB input limit."));
    }
    form.addEventListener("submit", event => { event.preventDefault(); perform(() => command({ action: "settings_save", ticket: editor.ticket, text: text.value })); });
    showDialog(editor.ticket, editor.namespace, form); return;
  }
  if (next.detail) {
    const detail = next.detail; const request = detail.request;
    const key = JSON.stringify([detail.pane, detail.generation, detail.kind,
      detail.kind === "approval" ? request.subject.session_id : null, request.id]);
    if (dialogKey === key) return;
    const form = element("form");
    const base = { pane: detail.pane, generation: detail.generation, id: request.id };
    if (detail.kind === "question") {
      const inputs = request.questions.map(question => {
        const field = element("div", "question-field"); const input = element("textarea"); input.rows = 2; input.required = true; input.setAttribute("aria-label", question.prompt);
        const choices = element("div", "actions");
        choices.append(...question.options.map(option => button(option, () => { input.value = option; input.focus(); })));
        field.append(element("label", "", question.prompt), choices, input); form.append(field); return input;
      });
      const send = element("button", "primary", "Send answers"); send.type = "submit"; form.append(send);
      form.addEventListener("submit", event => { event.preventDefault(); perform(() => command({ action: "answer", ...base, answers: inputs.map(input => input.value) })); });
      showDialog(key, "Answer the assistant", form);
    } else {
      form.append(element("p", "", request.reason), element("pre", "", JSON.stringify(request.review ?? request, null, 2)));
      const actions = element("div", "actions");
      actions.append(button("Allow once", () => command({ action: "approve", ...base, owner: request.subject.session_id, allow: true }), "primary"),
        button("Deny", () => command({ action: "approve", ...base, owner: request.subject.session_id, allow: false })));
      form.append(actions); showDialog(key, request.action, form);
    }
    return;
  }
  if (dialogKey && dialogKey !== "settings-prompt") { dialogKey = undefined; $("detail").close(); }
}


function renderUiDetail(detail) {
  const key = JSON.stringify(detail);
  if (dialogKey === key) return;
  const bound = detail.view;
  const formKey = JSON.stringify(bound);
  const previous = document.querySelector(".ui-contribution");
  const saved = new Map();
  if (previous?.dataset.formKey === formKey) {
    for (const input of previous.querySelectorAll("[data-ui-field]")) saved.set(input.dataset.uiField, input.value);
  }
  const body = element("div", "ui-contribution"); body.dataset.formKey = formKey;
  const fields = new Map();
  if (detail.error) body.append(element("p", "source-error", detail.error));
  for (const item of bound?.view.elements ?? []) {
    if (item.kind === "text" || item.kind === "code") {
      body.append(element(item.kind === "code" ? "pre" : "p", "ui-text", item.text));
    } else if (item.kind === "field") {
      const row = element("p", "ui-field"); row.append(element("strong", "", `${item.label}: `), document.createTextNode(item.value)); body.append(row);
    } else if (item.kind === "input") {
      const label = element("label", "ui-input", item.label);
      const input = element(item.multiline ? "textarea" : "input");
      input.setAttribute("aria-label", item.label); input.dataset.uiField = item.name;
      input.value = saved.get(item.name) ?? item.value; input.disabled = detail.busy;
      fields.set(item.name, input); label.append(input); body.append(label);
    } else if (item.kind === "button") {
      const action = button(item.label, () => command({ action: "ui_invoke", ticket: detail.ticket,
        reference: bound.actions[item.action], input: { value: item.value,
          fields: Object.fromEntries([...fields].map(([name, input]) => [name, input.value])) } }));
      action.disabled = detail.busy; body.append(action);
    }
  }
  if (detail.busy) body.append(element("p", "hint", "Working…"));
  showDialog(key, bound?.view.title ?? "Card details", body);
}
