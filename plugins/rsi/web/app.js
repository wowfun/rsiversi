import { downloadSession } from "./session-export.js";
import { lane, limits } from "./admission.js";
import { settingsForm, canUseSettingsForm } from "./settings-form.js";
import { publish, installActions, selectSurface } from "./src/bridge.ts";
import { NativeDocument } from "./src/native.ts";
import { MountTable } from "/mounts.js";
import { DraftStore, DraftEditor, validateEditor } from "/drafts.js";
import { openHistoryPicker } from "./history-picker.js";
import { openFilePicker } from "./file-picker.js";
import { externalPaneClass } from "./external-pane.js";
function referenceOrigin(meta) {
  return meta.source.kind === "native" ? `Session ${meta.source.binding.session_id}` : `${meta.source.owner}/${meta.source.id} · observed epoch ${meta.source.epoch}`;
}
function referenceInterval(meta) {
  const capture = meta.capture;
  return capture.kind === "suffix" ? {...capture.interval, first:String(BigInt(capture.interval.retained_after_seq)+1n), last:capture.interval.retained_through_seq} : {through_seq:capture.selection.through_seq,first:capture.selection.record.sequence,last:capture.selection.record.sequence,omissions:[]};
}
export function initialize() {
const $ = id => document.getElementById(id);
let pending = new Map();
let connection;
let connecting = false;
let worker;
let requestId = 0;
let view;
let mounts;
let rendererSlots = [];
let selected = "main";
let connected = false;
let closing = false;
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
function notify(message) {
  const storage = connection && !connection.closing ? connection.storageNotice : undefined;
  const text = [message, storage].filter(Boolean).join("\n");
  $("notice").textContent = text; $("notice").hidden = !text;
}
async function perform(run) {
  try { await run(); } catch (error) { notify(String(error.message ?? error)); }
}
function clearView() {
  publish(undefined); view = undefined; catalogKey = undefined; dialogKey = undefined; lastNotice = undefined;
  clearImages();
  for (const pane of panes.values()) pane.reset();
  ExternalPane.clearDrafts();
  $("detail").close();
}
function failWorker(error, current = connection) {
  if (current) {
    current.closing = true;
    current.authenticationDone?.();
    current.teardown ??= current.mounts.close();
    current.teardown.catch(cleanup => {
      if (connection === current) notify(`Renderer cleanup failed: ${cleanup.message}`);
    });
    for (const waiter of current.pending.values()) waiter.reject(new Error(error));
    current.pending.clear();
    current.worker.terminate();
  }
  if (current !== connection) return;
  connected = false; closing = true; worker = undefined;
  clearView();
  $("connection-state").textContent = "Connection failed";
  $("connection-state").classList.remove("connected");
  $("login").hidden = false;
  $("workbench").hidden = true;
  $("sign-out").hidden = true;
  notify(`${error}. Reconnect explicitly after renderer cleanup succeeds.`);
}

let frameId;
async function presentFrame(frame, assets, current = connection) {
  if (current && (connection !== current || current.closing)) return false;
  if (typeof frame.frame_id !== "string" || !/^[1-9][0-9]{0,19}$/.test(frame.frame_id) || BigInt(frame.frame_id) > 18446744073709551615n) throw new Error("Invalid presentation frame ID");
  let next;
  if (frame.kind === "snapshot") {
    next = frame.view;
  } else if (frame.kind === "patch") {
    if (!view || frame.base_frame_id !== frameId) return false;
    if (BigInt(frame.frame_id) !== BigInt(frameId) + 1n) return false;
    next = { ...view, ...frame.sections, surfaces: { ...view.surfaces } };
    for (const change of frame.surfaces) {
      if (!Object.hasOwn(next.surfaces, change.surface)) throw new Error("Invalid pane patch");
      const pane = { ...next.surfaces[change.surface], ...change.fields };
      if (change.transcript) {
        const delta = change.transcript;
        const blocks = new Map(pane.transcript.blocks.map(block => [block.key, block]));
        for (const key of delta.remove) blocks.delete(key);
        for (const block of delta.upsert) blocks.set(block.key, block);
        const order = delta.order ?? pane.transcript.blocks.map(block => block.key);
        if (new Set(order).size !== order.length || order.length !== blocks.size || order.some(key => !blocks.has(key))) throw new Error("Invalid block order");
        pane.transcript = { ...pane.transcript, ...delta.fields, blocks: order.map(key => blocks.get(key)) };
      }
      next.surfaces[change.surface] = pane;
    }
  } else { throw new Error("Unknown presentation frame"); }
  if (!next?.surfaces || Array.isArray(next.surfaces) || Object.keys(next.surfaces).length > 2 || Object.keys(next.surfaces).some(key => !/^[a-z0-9_-]{1,32}$/.test(key))) throw new Error("Invalid presentation snapshot");
  render(next);
  const renderer = await (current?.mounts ?? mounts).render(assets, rendererSlots);
  if (current && (connection !== current || current.closing)) return false;
  if (renderer?.error) notify(`Renderer update failed: ${renderer.error}`);
  frameId = frame.frame_id;
  return { accepted: true, renderer };
}
async function makeWorker() {
  const table = await MountTable.open();
  const current = { mounts: table, pending: new Map(), closing: false, worker: undefined };
  current.authenticated = new Promise(resolve => { current.authenticationDone = resolve; });
  try { current.worker = location.protocol === "rsi:" ? new NativeDocument() : new Worker("/worker.js", { type: "module" }); }
  catch (error) { await table.close(); throw error; }
  connection = current;
  mounts = table; pending = current.pending; worker = current.worker;
  frameId = undefined; closing = false;
  current.worker.onmessage = async ({ data }) => {
    if (connection !== current) return;
    if (data.kind === "view") {
      await current.authenticated;
      if (connection !== current || current.closing) return;
      try {
        const frame = typeof data.view === 'string' ? JSON.parse(data.view) : data.view;
        const assets = typeof data.assets === 'string' ? JSON.parse(data.assets) : data.assets;
        const presented = await presentFrame(frame, assets, current);
        if (!current.closing && connection === current) current.worker.postMessage({ kind: "ack", frame_id: frame.frame_id, resync: !presented?.accepted, renderer: presented?.renderer });
      } catch (error) { if (connection === current && !current.closing) failWorker(`View rendering failed: ${error.message}`, current); }
    } else if (data.kind === "reply") {
      const waiter = current.pending.get(data.id); current.pending.delete(data.id);
      if (data.error) waiter?.reject(Object.assign(new Error(data.error), { notAdmitted: data.notAdmitted === true, retryable: data.retryable !== false })); else waiter?.resolve(data.result);
    } else if (data.kind === "failed") { if (!current.closing) failWorker(data.error, current); }
  };
  current.worker.onerror = event => { event.preventDefault(); if (connection === current) failWorker("Browser Worker stopped", current); };
  return current;
}
function call(method, payload, transfer = []) {
  const selected = lane(method, payload);
  const lifecycle = selected === "lifecycle";
  if (closing && method !== "disconnect" && method !== "resources") return Promise.reject(Object.assign(new Error("The application is disconnecting"), { notAdmitted: true, retryable: false }));
  if (!worker) return Promise.reject(Object.assign(new Error("Connect to your service first"), { notAdmitted: true, retryable: false }));
  const inflight = [...pending.values()].filter(waiter => waiter.lane === selected).length;
  if (inflight >= limits[selected]) return Promise.reject(Object.assign(new Error("Input is busy; wait for the current action"), { notAdmitted: true }));
  const id = ++requestId;
  const waiters = pending;
  return new Promise((resolve, reject) => {
    waiters.set(id, { resolve, reject, lifecycle, lane: selected });
    try { worker.postMessage({ kind: "call", id, method, payload }, transfer); }
    catch (error) { waiters.delete(id); reject(Object.assign(error, { notAdmitted: true, retryable: false })); }
  });
}

function command(value) { return call("command", JSON.stringify(value)); }

async function connectWith(receipt) {
  if (connecting || (connection && !connection.closing)) throw new Error("A connection is already active or opening");
  connecting = true;
  $("connect").disabled = true; $("reconnect").disabled = true;
  notify(""); $("connection-state").textContent = "Connecting…";
  let current;
  try {
    current = await makeWorker();
    const identity = JSON.parse(await call("connect", { receipt, devHttp: $("dev-http").checked }));
    endpoint = identity.endpoint_id;
    try { current.drafts = await DraftStore.open(endpoint, identity.principal); }
    catch (error) {
      current.drafts = new DraftStore(undefined, endpoint, identity.principal.kind === "local" ? "local" : `device:${identity.principal.device_id}`);
      current.storageNotice = `Draft storage is unavailable. Input cannot be saved or sent: ${error.message}`;
    }
    current.authenticationDone();
    if (connection !== current || current.closing) return;
    try { localStorage.setItem("rsi.endpoint", endpoint); } catch { /* Storage is optional. */ }
    connected = true;
    $("connection-state").textContent = "Connected";
    $("connection-state").classList.add("connected");
    $("login").hidden = true; $("workbench").hidden = false; $("sign-out").hidden = false;
    $("reconnect").hidden = false;
    if (current.storageNotice) notify("");
  } catch (error) {
    if (current) { current.authenticationDone(); failWorker(error.message, current); }
    else notify(error.message);
  } finally {
    connecting = false;
    if (!current || connection === current) { $("connect").disabled = false; $("reconnect").disabled = false; }
  }
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
async function disconnectDocument() {
  if (closing) return;
  $("sign-out").disabled = true;
  try {
    let resources;
    closing = true;
    const current = connection;
    if (current) current.closing = true;
    try {
      const drafts = await Promise.allSettled([...panes.values()].map(pane => pane.flush(true)));
      const failed = drafts.find(result => result.status === "rejected");
      if (failed) {
        closing = false;
        if (current) current.closing = false;
        if (location.protocol === "rsi:") {
          const cancelled = await fetch("/_close_cancel", {method:"POST",body:""});
          if (!cancelled.ok) throw new Error("Could not cancel native close after a failed draft save");
        }
        notify(`Draft is not saved: ${failed.reason?.message ?? failed.reason}. Recover the draft before closing.`);
        return;
      }
      await mounts.close();
      resources = await call("disconnect", true);
    }
    catch (error) { failWorker(String(error.message ?? error)); return; }
    if (current && connection !== current) return;
    document.dispatchEvent(new CustomEvent("rsi-disconnected", { detail: resources }));
    connected = false;
    worker.terminate(); worker = undefined;
    clearView();
    $("connection-state").textContent = "Disconnected";
    $("connection-state").classList.remove("connected");
    $("workbench").hidden = true; $("login").hidden = false; $("sign-out").hidden = true;
    notify("");
  } finally { $("sign-out").disabled = false; }
}
$("sign-out").addEventListener("click", () => perform(disconnectDocument));
window.addEventListener("rsi-native-close", () => perform(disconnectDocument));

function draftRecovery(editor, resolved = () => {}) {
  const choose = useSaved => async () => { await editor.resolve(useSaved); await resolved(); };
  return [element("p", "draft-error", `Input is not saved. ${editor.failure?.message ?? "Resolve this tab's input before recovering the conversation."}`),
    button("Use saved input", choose(true), "quiet"),
    button("Replace saved text and images", choose(false), "quiet")];
}

class Pane {
  constructor(index) {
    this.index = index;
    this.blocks = new Map();
    this.visibleSequence = 0n;
    this.generation = undefined;
    this.editors = new Map();
    this.enterSubmit = false;
    this.images = [];
    this.node = element("section", "pane");
    this.node.setAttribute("aria-label", `${index === "compare" ? "Compare" : "Main"} conversation`);
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
    this.restore = button("Restore drafts", () => this.restoreDrafts(), "quiet");
    tools.append(this.history, this.live, this.commands, this.uiMenu, this.restore);
    this.commandView = element("div", "session-commands");
    this.extensionView = element("details", "session-extensions");
    this.extensionView.setAttribute("aria-label", "Extension state");
    this.transcript = element("div", "transcript");
    this.transcript.setAttribute("aria-label", "Conversation transcript");
    this.transcript.tabIndex = 0;
    this.transcript.addEventListener("scroll", () => this.scheduleVisible(), { passive: true });
    this.resizeObserver = new ResizeObserver(() => this.scheduleVisible());
    this.resizeObserver.observe(this.transcript);
    this.waiting = element("div", "pending");
    this.notice = element("div", "pane-notice");
    this.composer = element("form", "composer");
    this.input = element("textarea");
    this.input.setAttribute("aria-label", `${index === "compare" ? "Compare" : "Main"} message`);
    this.input.placeholder = "Describe the work…";
    this.input.maxLength = 1024 * 1024;
    this.input.addEventListener("compositionstart", () => { this.composing = true; this.closeCompletions(); });
    this.input.addEventListener("compositionend", () => { this.composing = false; this.updateCompletions(); });
    this.input.addEventListener("input", () => {
      try { this.edit(this.input.value); this.updateCompletions(); }
      catch (error) { this.input.value = this.editor?.text ?? ""; notify(error.message); }
    });
    this.input.addEventListener("keydown", event => {
      if (event.isComposing || event.keyCode === 229) return;
      if (this.completionKey(event)) return;
      if (event.key === "Enter" && !event.shiftKey && (event.metaKey || event.ctrlKey || this.enterSubmit)) { event.preventDefault(); perform(() => this.submit(false)); }
    });
    this.input.addEventListener("click", () => this.updateCompletions());
    this.input.addEventListener("keyup", event => { if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) this.updateCompletions(); });
    this.completionPanel = element("section", "input-completions");
    this.completionPanel.hidden = true;
    this.completionPanel.setAttribute("aria-label", "Commands, skills and agents");
    this.resourcePreview = element("section", "input-resource-preview");
    this.resourcePreview.hidden = true;
    this.composer.addEventListener("submit", event => { event.preventDefault(); perform(() => this.submit(false)); });
    const bar = element("div", "composer-bar");
    this.model = element("select"); this.model.setAttribute("aria-label", `${index === "compare" ? "Compare" : "Main"} model`);
    this.model.addEventListener("change", () => perform(() => this.action("model", { model: JSON.parse(this.model.value), reasoning_effort: null })));
    this.model.addEventListener("focus", () => perform(() => this.action("model_refresh")));
    this.effort = element("select");
    this.effort.setAttribute("aria-label", `${index === "compare" ? "Compare" : "Main"} reasoning effort`);
    this.effort.addEventListener("change", () => perform(() => this.action("model", {
      model: JSON.parse(this.model.value), reasoning_effort: this.effort.value || null,
    })));
    this.modelReceipt = button("Check model change", () => this.action("model_refresh"), "quiet");
    this.modelReceipt.hidden = true;
    const actions = element("div", "actions");
    this.imageInput = element("input"); this.imageInput.type = "file"; this.imageInput.accept = "image/*"; this.imageInput.multiple = true;
    this.imageInput.hidden = true; this.imageInput.setAttribute("aria-label", `${index === "compare" ? "Compare" : "Main"} image files`);
    this.imageInput.addEventListener("change", () => {
      const files = [...this.imageInput.files]; this.imageInput.value = "";
      perform(() => this.upload(files));
    });
    this.attach = button("Add images", () => this.imageInput.click(), "quiet");
    this.addReference = button("Reference session", () => this.showReferencePicker(), "quiet");
    this.exportButton = button("Export", () => this.exportSession(""), "quiet");
    this.exportAbort = undefined;
    this.searchHistory = button("Search history", () => openHistoryPicker(this, view, call, button), "quiet");
    this.addFile = button("@ File path", () => openFilePicker(this, call, button), "quiet");
    this.referenceList = element("div", "draft-references");
    this.referenceList.setAttribute("aria-label", "Conversation references");
    this.imageList = element("div", "draft-images");
    this.frozenImages = element("p", "hint frozen-images");
    this.draftStatus = element("div", "draft-status");
    this.recovery = element("div", "draft-recovery");
    this.recovery.hidden = true;
    this.cancel = button("Cancel", () => this.action("cancel"), "quiet");
    this.steer = button("Steer", () => this.submit(true));
    this.send = button("Send ↗", () => this.submit(false), "primary");
    actions.append(this.attach, this.addFile, this.addReference, this.searchHistory, this.exportButton, this.cancel, this.steer, this.send); bar.append(this.model, this.effort, this.modelReceipt, actions);
    this.hint = element("div", "composer-hint", "Ctrl / ⌘ Enter to send · Enter for a new line");
    this.composer.append(this.resourcePreview, this.completionPanel, this.referenceList, this.input, this.imageInput, this.imageList, this.frozenImages, this.draftStatus, bar, this.hint);
    this.node.append(header, tools, this.commandView, this.recovery, this.extensionView, this.transcript, this.waiting, this.notice, this.composer);
    $("panes").append(this.node);
    this.render(null, []);
  }
  action(action, fields = {}) {
    if (!this.generation) throw new Error("Open a conversation first");
    return command({ action, pane: this.index, generation: this.generation, ...fields });
  }
  scheduleVisible() {
    if (this.visibleFrame) return;
    this.visibleFrame = requestAnimationFrame(() => { this.visibleFrame = undefined; this.syncVisible(); });
  }
  syncVisible() {
    if (!this.generation || !this.node.isConnected || closing) return;
    const bounds = this.transcript.getBoundingClientRect();
    const entries = [...this.blocks].filter(([, entry]) => {
      const rect = entry.node.getBoundingClientRect();
      return this.uiCards && bounds.height > 0 && rect.bottom > bounds.top && rect.top < bounds.bottom;
    }).slice(-4);
    const key = JSON.stringify([this.generation, this.historical, this.uiRevision, entries.map(([key, entry]) => [key, entry.sourceKey])]);
    if (key === this.visibleKey) return;
    this.visibleKey = key;
    this.visiblePending = { generation: this.generation, sequence: (++this.visibleSequence).toString(), keys: entries.map(([key]) => key) };
    if (!this.visibleRunning) void this.sendVisible();
  }
  async sendVisible() {
    this.visibleRunning = true;
    try {
      while (this.visiblePending && !closing) {
        const request = this.visiblePending; this.visiblePending = undefined;
        for (let attempt = 0; attempt < 2; attempt++) {
          try { await command({ action: "ui_visible", pane: this.index, ...request }); break; }
          catch (error) {
            if (this.generation !== request.generation || closing) break;
            if (attempt === 1) notify(error.message);
            else await new Promise(resolve => setTimeout(resolve, 100));
          }
        }
      }
    } finally { this.visibleRunning = false; }
  }
  renderInline(cards) {
    for (const [key, entry] of this.blocks) {
      const card = cards?.[key];
      entry.inline.hidden = !card;
      if (!card?.model || !card.ticket) {
        if (card) entry.inline.textContent = card.error ?? "Loading card…";
        continue;
      }
      rendererSlots.push({ key: `inline-${this.index}-${card.binding.epoch}`, surface: "pane", root: entry.inline,
        binding: JSON.stringify(card.binding), snapshot: { model: card.model, busy: card.busy, error: card.error },
        host: { invoke(action, input) { return command({ action: "ui_invoke", ticket: card.ticket, name: action, input }); },
          source(name, offset, maximum) { return call("ui_source", JSON.stringify({ ticket: card.ticket, name, offset, maximum })); } }
      });
    }
  }
  edit(text, images = this.editor?.images ?? [], references = this.editor?.references ?? []) {
    if (!this.editor) throw new Error("The saved draft is still loading");
    const textBytes = validateEditor(text, images, references);
    const total = [...this.editors.values()].reduce((sum, editor) => sum + (editor === this.editor ? textBytes : editor.textBytes), 0);
    if (total > 2 * 1024 * 1024) throw new Error("Local drafts exceed this pane's 2 MiB limit");
    this.editor.edit(text, images, references);
  }
  async bindDraft(data) {
    const generation = this.generation, current = connection;
    this.editor = undefined;
    if (!data || !current?.drafts) return;
    const store = current.drafts, key = JSON.stringify(store.key(this.index, data.session));
    let editor = this.editors.get(key);
    if (!editor?.dirty && !editor?.failure) {
      if (!editor && this.editors.size >= 64) {
        for (const [key, entry] of this.editors) if (!entry.dirty && !entry.failure && !entry.saving) this.editors.delete(key);
        if (this.editors.size >= 64) throw new Error("Local draft capacity is full; save or clear an unsaved draft first");
      }
      let record, failure;
      try { record = await store.ensure(this.index, data.session, data.header, data.creation); }
      catch (error) { failure = error; record = editor?.record ?? store.blank(this.index, data.session, data.header, data.creation); }
      editor = new DraftEditor(store, record);
      editor.failure = failure;
      this.editors.set(key, editor);
    }
    if (this.generation !== generation || connection !== current || current.closing) return;
    this.editor = editor;
    this.bindingError = editor.record.header !== data.header ? "Saved Session Header changed. The draft is retained and cannot be sent to this Session." : undefined;
    editor.changed = () => { if (this.editor === editor) this.renderComposer(); };
    this.renderComposer();
    if (editor.record.pending && editor.record.pending.phase !== "prepared" && !editor.failure && !this.bindingError) {
      void this.reconcile("query").catch(error => notify(error.message));
    }
  }
  async flush(all = false) {
    await this.binding;
    const editors = all ? [...this.editors.values()] : this.editor ? [this.editor] : [];
    const results = await Promise.allSettled(editors.map(editor => editor.flush()));
    const failure = results.find(result => result.status === "rejected");
    if (failure) throw failure.reason;
  }
  renderComposer() {
    const editor = this.editor, pending = editor?.record.pending;
    if (this.input.value !== (editor?.text ?? "")) this.input.value = editor?.text ?? "";
    this.input.disabled = !this.generation || !editor || editor.transferring || this.switching;
    this.send.disabled = !editor || !!editor.failure || editor.transferring || !!this.bindingError || this.submitting || this.uploading || this.switching;
    this.steer.disabled = this.send.disabled || !!pending;
    const sendLabel = pending ? (pending.phase === "prepared" ? "Send saved request" : pending.kind === "message" ? "Retry previous" : "Check command result") : "Send ↗";
    if (this.send.textContent !== sendLabel) this.send.textContent = sendLabel;
    this.draftStatus.replaceChildren();
    if (this.bindingError) this.draftStatus.append(element("p", "draft-error", this.bindingError));
    if (editor?.failure) {
      this.draftStatus.append(...draftRecovery(editor));
    } else if (editor?.dirty) this.draftStatus.append(element("p", "hint", "Saving draft…"));
    if (pending) {
      this.draftStatus.append(element("p", "hint", `${pending.kind === "command" ? "Command" : "Message"} ${pending.id} · ${pending.phase === "prepared" ? "Saved before execution" : "Awaiting confirmation"}`));
      if (pending.references) this.draftStatus.append(element("p", "hint", `Saved request includes ${pending.references} frozen conversation reference${pending.references === 1 ? "" : "s"}. Later draft edits apply to the next request.`));
      if (pending.phase === "prepared") this.draftStatus.append(button("Cancel saved request", () => editor.update(record => editor.store.cancelPrepared(record)), "quiet"));
      else this.draftStatus.append(button("Check previous result", () => this.reconcile("query"), "quiet"));
    }
    this.renderImages(view?.surfaces[this.index]);
    this.renderReferences();
    this.renderCommands(view?.surfaces[this.index]);
  }
  renderReferences() {
    const editor = this.editor;
    this.addReference.disabled = !editor || !!this.bindingError || editor.transferring || this.switching || editor.references.length >= 4;
    this.searchHistory.disabled = !editor || !!this.bindingError || editor.transferring || this.switching || editor.references.length >= 4;
    this.addFile.disabled = !editor || !!this.bindingError || editor.transferring || this.switching;
    const key = `${this.generation}:${editor?.referencesRevision}`;
    if (editor === this.referenceEditor && key === this.referenceKey) return;
    this.referenceEditor = editor;
    this.referenceKey = key; this.referenceList.replaceChildren();
    for (const reference of editor?.references ?? []) {
      const row = element("div", "reference-row");
      const source = referenceOrigin(reference.metadata), interval = referenceInterval(reference.metadata);
      row.append(element("span", "reference-origin", `${source} · through record ${interval.through_seq}${interval.omissions.length ? " · shortened" : ""}`),
        button("Preview reference", () => this.showReferencePicker(reference), "quiet"),
        button("Remove reference", () => { this.edit(this.editor.text, this.editor.images, this.editor.references.filter(item => item.snapshot.sha256 !== reference.snapshot.sha256)); this.input.focus(); }, "quiet"));
      this.referenceList.append(row);
    }
    this.referenceList.hidden = !editor?.references.length;
  }
  showReferencePicker(existing) {
    const editor = this.editor, generation = this.generation;
    if (!editor || !generation || this.switching) throw new Error("Open a conversation first");
    this.referenceDialog?.close();
    const dialog = element("dialog", "detail-dialog reference-dialog");
    dialog.setAttribute("aria-label", existing ? "Frozen conversation reference" : "Reference a conversation");
    this.referenceDialog = dialog;
    const heading = element("div", "dialog-heading");
    heading.append(element("h2", "", existing ? "Frozen conversation reference" : "Reference a conversation"), button("Close", () => dialog.close(), "quiet"));
    const help = element("p", "hint", "Capture human and assistant text from a saved conversation. The reference keeps these bytes when the source changes.");
    const source = element("input"); source.type = "text"; source.placeholder = "Session ID"; source.setAttribute("aria-label", "Source Session ID"); source.maxLength = 256;
    const choices = element("select"); choices.setAttribute("aria-label", "Recent source conversations");
    const initial = element("option", "", "Choose a visible conversation…"); initial.value = ""; choices.append(initial);
    for (const entry of view?.navigation?.entries ?? []) { const option = element("option", "", entry.metadata.title ?? entry.session); option.value = entry.session; choices.append(option); }
    choices.addEventListener("change", () => { source.value = choices.value; });
    const status = element("p", "hint"); status.setAttribute("role", "status");
    const content = element("section", "reference-preview-content");
    const actions = element("div", "actions");
    let request = 0;
    const alive = () => dialog.open && this.generation === generation && this.editor === editor && this.referenceDialog === dialog;
    const show = (reference, page) => {
      content.replaceChildren();
      const meta = reference.metadata, interval = referenceInterval(meta);
      content.append(element("p", "reference-origin", `${referenceOrigin(meta)} · through record ${interval.through_seq}`),
        element("p", "hint", `Retained records ${interval.first}–${interval.last} · ${meta.text_bytes} text bytes`));
      if (interval.omissions.length) content.append(element("p", "hint", `Earlier material omitted: ${interval.omissions.map(reason => ({ fact_limit:"capture reached 1,024 Facts", scan_bytes:"capture reached 16 MiB of source Facts", content_bytes:"text reached 1 MiB" })[reason]).join("; ")}.`));
      content.append(element("pre", "", page?.text ?? reference.preview));
      const offset = page?.offset ?? 0, next = page?.next_offset ?? new TextEncoder().encode(reference.preview).length;
      if (offset > 0) content.append(button("Previous reference page", () => preview(reference, Math.max(0, offset - 8192)), "quiet"));
      if (page?.has_more ?? next < meta.text_bytes) content.append(button("Next reference page", () => preview(reference, next), "quiet"));
      const add = button("Add to draft", () => {
        if (!alive()) return;
        this.edit(editor.text, editor.images, [...editor.references, reference]); dialog.close();
      }, "primary");
      add.disabled = editor.references.some(item => item.snapshot.sha256 === reference.snapshot.sha256) || editor.references.length >= 4;
      if (!existing) content.append(add);
    };
    const preview = async (reference, offset) => {
      const revision = ++request; status.textContent = "Reading frozen reference…";
      try {
        const page = JSON.parse(await call("reference_input", JSON.stringify({ pane:this.index, generation, operation:{kind:"preview",reference,offset,maximum:8192} })));
        if (!alive() || request !== revision) return;
        status.textContent = ""; show(reference, page);
      } catch (error) { if (alive() && request === revision) status.textContent = `Reference unavailable: ${error.message}. Your draft is unchanged.`; }
    };
    const capture = button("Capture preview", async () => {
      const revision = ++request; capture.disabled = true; content.replaceChildren(); status.textContent = "Capturing conversation…";
      try {
        const reference = JSON.parse(await call("reference_input", JSON.stringify({pane:this.index,generation,operation:{kind:"capture",source:source.value.trim()}})));
        if (!alive() || revision !== request) return;
        status.textContent = "Captured. Review it before adding to the draft."; show(reference);
      } catch (error) { if (alive() && revision === request) status.textContent = `Capture failed: ${error.message}. Your draft is unchanged.`; }
      finally { if (alive()) capture.disabled = false; }
    }, "primary");
    actions.append(capture);
    dialog.append(heading,help);
    if (!existing) dialog.append(choices,source,actions);
    dialog.append(status,content);
    dialog.addEventListener("close", () => { request++; if (this.referenceDialog === dialog) this.referenceDialog = undefined; dialog.remove(); if (this.generation === generation && this.editor === editor) this.input.focus(); });
    document.body.append(dialog); dialog.showModal();
    if (existing) show(existing); else source.focus();
  }
  async exportSession(args) {
    if (this.exportAbort) { this.exportAbort.abort(); return; }
    if (!this.generation || !this.editor) throw new Error("Open a native conversation first");
    const abort = new AbortController(), generation = this.generation;
    this.exportAbort = abort; this.exportButton.textContent = "Cancel export";
    const closed = () => abort.abort(); window.addEventListener("pagehide",closed,{once:true});
    try { await downloadSession(call,this.index,generation,args,abort.signal); }
    finally { window.removeEventListener("pagehide",closed); this.exportAbort = undefined; this.exportButton.textContent = "Export"; }
  }
  async submit(steer) {
    if (this.submitting || this.uploading) return;
    if (this.editor && /^\s*\/export(?:\s|$)/.test(this.editor.text)) {
      if (this.editor.images.length || this.editor.references.length) throw new Error("Export does not accept attachments");
      const text = this.editor.text;
      await this.exportSession(text.replace(/^\s*\/export/, ""));
      return;
    }
    this.submitting = true; this.renderComposer();
    try {
      await this.flush();
      if (!this.editor || this.bindingError) throw new Error(this.bindingError ?? "Open a conversation first");
      const editor = this.editor, generation = this.generation, current = connection;
      if (!editor.record.pending) {
        const captured = editor.record;
        const prepared = JSON.parse(await call("prepare_submission", JSON.stringify({ pane: this.index, generation, text: editor.text, images: editor.images, references: editor.references, steer })));
        if (connection !== current || current.closing || this.editor !== editor) throw new Error("Conversation changed before request preparation completed");
        await editor.update(() => editor.store.freeze(captured, prepared));
      }
      const mode = editor.record.pending.phase === "prepared" ? "dispatch" : editor.record.pending.kind === "message" ? "retry_message" : "query";
      await this.executePending(editor, generation, current, mode);
    } finally { this.submitting = false; this.renderComposer(); }
  }
  async reconcile(mode) {
    if (this.submitting || !this.editor || this.bindingError) return;
    const editor = this.editor, generation = this.generation, current = connection;
    this.submitting = true; this.renderComposer();
    try { await editor.flush(); await this.executePending(editor, generation, current, mode); }
    finally { this.submitting = false; this.renderComposer(); }
  }
  async executePending(editor, generation, current, mode) {
    if (!editor.record.pending) return;
    if (mode === "dispatch") await editor.update(record => editor.store.begin(record));
    const expected = editor.record;
    let result;
    if (connection !== current || current.closing || this.editor !== editor) {
      result = { status: mode === "dispatch" ? "not_admitted" : "unknown", error: "Connection changed before dispatch" };
    } else {
      try { result = JSON.parse(await call("dispatch_submission", { pane: this.index, generation, opaque: expected.pending.opaque, mode })); }
      catch (error) { result = { status: error.notAdmitted && mode === "dispatch" ? "not_admitted" : "unknown", error: error.message }; }
    }
    await editor.update(() => editor.store.settle(expected, result));
    if (result.status !== "complete") throw new Error(result.error ?? "The original submission still needs confirmation");
  }
  async upload(files) {
    if (!files.length) return;
    if (this.uploading || !this.generation || !this.editor) throw new Error("Image import is unavailable while this pane is busy");
    const limits = view.media_limits, editor = this.editor, generation = this.generation, current = connection;
    if (editor.images.length + files.length > limits.images) throw new Error(`A draft can hold at most ${limits.images} images`);
    if (files.some(file => !file.size || file.size > limits.upload_bytes)) throw new Error("Each image source must contain 1 byte to 16 MiB");
    this.uploading = true; this.renderComposer();
    try {
      for (const file of files) {
        const bytes = await file.arrayBuffer();
        if (connection !== current || current.closing || this.generation !== generation) throw new Error("Pane changed; remaining images were not imported");
        const media = JSON.parse(await call("import_image", { pane: this.index, generation, bytes }, [bytes]));
        editor.edit(editor.text, [...editor.images, media]);
        await editor.flush();
      }
    } finally { this.uploading = false; this.renderComposer(); }
  }
  async restoreDrafts() {
    const current = connection, store = current?.drafts;
    if (!store) throw new Error("Connect to your service first");
    const records = await store.list(this.index);
    if (connection !== current || current.closing) return;
    this.recovery.hidden = false;
    this.recovery.replaceChildren(element("p", "hint", "Saved drafts for this device"), button("Close saved drafts", () => { this.recovery.hidden = true; }, "quiet"));
    for (const record of records) {
      const row = element("div", "saved-draft");
      const open = button("Open saved conversation", async () => {
        await this.flush().catch(() => {});
        this.switching = true; this.renderComposer();
        try {
          const result = JSON.parse(await call("restore_session", JSON.stringify({ action: "open", pane: this.index, session: record.key[3], header: record.header })));
          if (result.status === "expired") {
            row.append(element("p", "hint", "The original Session is no longer available. Your input is still saved."));
            if (record.creation && !record.everDispatched && !record.pending && !row.querySelector(".recreate")) {
              row.append(button("Start a new conversation with this draft", async () => {
                await this.recreateDraft(record, current); row.remove();
              }, "quiet recreate"));
            }
          } else { this.recovery.hidden = true; }
        } finally { this.switching = false; this.renderComposer(); }
      }, "quiet");
      row.append(element("span", "", `${record.key[3]} · ${record.pending ? "awaiting confirmation" : `${record.text.slice(0, 80)}${record.images.length ? ` · ${record.images.length} images` : ""}`}`), open);
      const local = this.editors.get(JSON.stringify(record.key));
      if (local?.dirty || local?.failure) {
        const input = element("textarea"); input.readOnly = true; input.rows = 3; input.value = local.text;
        input.setAttribute("aria-label", "Unsaved recovered input");
        row.append(input, ...draftRecovery(local, () => this.restoreDrafts()));
      }
      if (!record.text && !record.images.length && !record.pending) row.append(button("Remove empty draft", async () => { await store.removeEmpty(record); row.remove(); }, "quiet"));
      this.recovery.append(row);
    }
    if (!records.length) this.recovery.append(element("p", "hint", "No saved drafts"));
  }
  async recreateDraft(record, current) {
    if (connection !== current || current.closing) throw new Error("Connection changed; the draft was left untouched");
    if (this.recreating || this.submitting || this.uploading || this.switching) throw new Error("Wait for this pane's current operation before recovering the draft");
    const store = current.drafts, key = JSON.stringify(record.key);
    const source = this.editors.get(key) ?? new DraftEditor(store, record);
    this.recreating = true; this.switching = true; this.renderComposer();
    try {
      await source.transfer(async expected => {
        if (source.record.incarnation !== record.incarnation) throw new Error("The saved draft was replaced; reopen the recovery list");
        if (!expected.creation || expected.everDispatched || expected.pending) throw new Error("This draft is no longer eligible for a new conversation");
        if (connection !== current || current.closing) throw new Error("Connection changed; the draft was left untouched");
        const fresh = JSON.parse(await call("restore_session", JSON.stringify({ action: "create", pane: this.index, creation: expected.creation })));
        const blank = await store.ensure(this.index, fresh.session, fresh.header, fresh.creation);
        const moved = await store.moveFresh(expected, blank);
        await this.binding;
        if (connection === current && this.editor && JSON.stringify(this.editor.record.key) === JSON.stringify(moved.key)) {
          if (this.editor.dirty || this.editor.failure) { this.editor.failure = new Error("Saved input was restored while you were typing; choose which editor to keep"); }
          else { this.editor.record = moved; this.editor.text = moved.text; this.editor.images = structuredClone(moved.images); this.editor.references = structuredClone(moved.references); }
        }
        if (this.editor === source) this.editor = undefined;
        this.editors.delete(key);
      });
    } finally { this.recreating = false; this.switching = false; this.renderComposer(); }
  }
  renderImages(data) {
    this.images = this.editor?.images ?? [];
    this.retryImages = this.editor?.record.pending?.images;
    this.attach.hidden = !view?.has_media;
    this.attach.disabled = !data || !this.editor || this.uploading || this.submitting || this.switching;
    this.frozenImages.textContent = this.retryImages ? `Previous submission retains ${this.retryImages} image(s) in its original order. Draft changes apply to the next submission.` : "";
    const key = JSON.stringify([this.generation, this.editor?.revision, this.images]);
    if (key === this.imagesKey) return;
    this.imagesKey = key;
    this.imageList.replaceChildren(...this.images.map((media, index) => {
      const row = element("div", "draft-image");
      row.append(element("span", "", `${index + 1}. ${media.width} × ${media.height} · ${media.bytes} bytes`));
      const edit = to => { const images = [...this.editor.images]; const [media] = images.splice(index, 1); if (to !== null) images.splice(to, 0, media); this.edit(this.editor.text, images); };
      const earlier = button("Move image earlier", () => edit(index - 1), "quiet"); earlier.disabled = index === 0;
      const later = button("Move image later", () => edit(index + 1), "quiet"); later.disabled = index + 1 === this.images.length;
      row.append(button("Preview image", () => this.action("inspect_image", { media }), "quiet"), earlier, later, button("Remove image", () => edit(null), "quiet"));
      return row;
    }));
  }
  reset() { this.exportAbort?.abort(); this.referenceDialog?.close(); this.fileDialog?.close(); this.recovery.hidden = true; this.recovery.replaceChildren(); this.editor = undefined; this.generation = undefined; this.input.value = ""; this.render(null, []); }
  completionToken() {
    const text = this.input.value, cursor = this.input.selectionStart;
    if (this.composing || !this.generation || this.switching || this.input.selectionEnd !== cursor) return;
    const before = text.slice(0, cursor), match = /(?:^|[\s([{])(@[A-Za-z0-9_.-]*)$/.exec(before);
    if (match) {
      const start = cursor - match[1].length, suffix = /^[A-Za-z0-9_.-]*/.exec(text.slice(cursor))[0];
      const end = cursor + suffix.length;
      const name = text.slice(start + 1, end), trailing = text[end];
      const locator = trailing === '/' || trailing === ':' && (name === 'path_hex' || end + 1 < text.length && !/\s/u.test(text[end + 1]));
      if ((!name || /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/.test(name)) && !locator && !(before.split("```").length % 2 === 0) && !(before.split("`").length % 2 === 0))
        return { query: text.slice(start,end), start, end, text, cursor, agent: true };
    }
    if (!text.startsWith("/") || text.startsWith("//") || /[\r\n]/.test(text)) return;
    const end = text.search(/\s/), stop = end < 0 ? text.length : end;
    if (cursor > stop) return;
    return { query: text.slice(1, stop), start: 0, end: stop, text, cursor, agent: false };
  }

  updateCompletions(refresh = false) {
    const token = this.completionToken();
    if (!token) { this.closeCompletions(); this.completionRequestKey = undefined; return; }
    refresh ||= !this.completionState || this.completionState.refreshPending;
    const key = JSON.stringify([this.generation, token.text, token.cursor]);
    if (!refresh && key === this.completionRequestKey) return;
    this.completionRequestKey = key;
    this.completionFailure = undefined;
    this.completionState = { ...token, sequence: String(this.completionSequence = (this.completionSequence ?? 0) + 1), generation: this.generation, selected: 0, refreshPending: refresh };
    const state = this.completionState;
    clearTimeout(this.completionTimer);
    this.renderCompletions(view?.surfaces[this.index]);
    this.completionTimer = setTimeout(() => {
      if (this.completionState !== state) return;
      state.refreshPending = false;
      void this.action("completions", { query: state.query, sequence: state.sequence, refresh }).catch(error => {
        if (this.completionState === state) { this.completionFailure = error.message; this.renderCompletions(view?.surfaces[this.index]); }
      });
    }, 80);
  }
  closeCompletions() {
    clearTimeout(this.completionTimer);
    this.completionState = undefined; this.completionRenderKey = undefined; this.completionPanel.hidden = true;
    this.input.removeAttribute("aria-activedescendant"); this.input.setAttribute("aria-expanded", "false");
    this.input.removeAttribute("aria-controls");
  }
  completionEntries(data = view?.surfaces[this.index]) {
    const state = this.completionState, result = data?.completions;
    if (!state || state.generation !== this.generation || result?.sequence !== state.sequence || result.query !== state.query) return;
    return result.entries;
  }
  pickCompletion(item) {
    const state = this.completionState, token = this.completionToken();
    if (!state || !token || state.text !== token.text || state.cursor !== token.cursor || state.generation !== this.generation) return;
    this.input.setRangeText(item.replacement, token.start, token.end, "end");
    this.edit(this.input.value);
    this.completionRequestKey = JSON.stringify([this.generation, this.input.value, this.input.selectionStart]);
    this.closeCompletions();
    this.input.focus();
  }
  completionKey(event) {
    if (event.key === "Escape" && !this.resourcePreview.hidden) {
      event.preventDefault(); perform(() => this.action("resource_close")); this.input.focus(); return true;
    }
    if (!this.completionState || this.completionPanel.hidden) return false;
    if (event.key === "Escape") {
      event.preventDefault(); this.closeCompletions();
      return true;
    }
    const entries = this.completionEntries();
    if (!entries?.length) return false;
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      this.completionState.selected = Math.max(0, Math.min(entries.length - 1, this.completionState.selected + (event.key === "ArrowDown" ? 1 : -1)));
      this.renderCompletions(view?.surfaces[this.index]);
      this.completionPanel.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
      return true;
    }
    if ((event.key === "Tab" || event.key === "Enter") && !event.shiftKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault(); this.pickCompletion(entries[this.completionState.selected]); return true;
    }
    if (event.key === "F2" && entries[this.completionState.selected].resource) {
      event.preventDefault(); perform(() => this.action("resource_read", { request: entries[this.completionState.selected].resource })); return true;
    }
    return false;
  }
  renderCompletions(data) {
    const state = this.completionState;
    if (!state || state.generation !== this.generation) { this.closeCompletions(); return; }
    const entries = this.completionEntries(data);
    // One completed response is immutable for its request sequence.
    const key = JSON.stringify([state.generation, state.sequence, state.selected, !!entries, this.completionFailure, data?.completions?.sequence === state.sequence ? data.completions.notice : null]);
    if (key === this.completionRenderKey) return;
    this.completionRenderKey = key;
    this.completionPanel.hidden = false;
    this.input.setAttribute("aria-expanded", "true");
    this.input.setAttribute("aria-autocomplete", "list");
    const list = element("div", "completion-options");
    list.id = `completion-${this.index}`; list.setAttribute("role", "listbox");
    this.input.setAttribute("aria-controls", list.id);
    this.input.removeAttribute("aria-activedescendant");
    let group;
    for (const [index, item] of (entries ?? []).entries()) {
      if (group !== item.group) { group = item.group; const heading = element("div", "completion-group", group === "agent" ? "Agents" : group === "skill" ? "Skills" : "Commands"); heading.setAttribute("role", "presentation"); list.append(heading); }
      const row = button(item.replacement, () => this.pickCompletion(item), "completion-option");
      row.id = `${list.id}-${index}`; row.tabIndex = -1; row.setAttribute("role", "option");
      row.setAttribute("aria-selected", String(state.selected === index));
      row.append(element("span", "completion-description", item.description));
      row.addEventListener("mousedown", event => event.preventDefault());
      list.append(row);
      if (state.selected === index) this.input.setAttribute("aria-activedescendant", row.id);
    }
    if (!entries?.length) { const status = element("p", "hint", this.completionFailure ?? (entries ? "No matching entries" : "Loading entries…")); status.setAttribute("role", "status"); list.append(status); }
    const tools = element("div", "completion-tools");
    tools.append(element("span", "hint", "↑ ↓ choose · Enter / Tab insert · F2 preview · Esc close"));
    const selected = entries?.[state.selected];
    if (selected?.resource) tools.append(button(selected.group === "agent" ? "Preview agent" : "Preview skill", () => this.action("resource_read", { request: selected.resource }), "quiet"));
    tools.append(button("Refresh", () => this.updateCompletions(true), "quiet"));
    const scroll = this.completionPanel.querySelector(".completion-options")?.scrollTop ?? 0;
    this.completionPanel.replaceChildren(list, tools);
    list.scrollTop = scroll;
    if (data?.completions?.sequence === state.sequence && data.completions.notice) this.completionPanel.append(element("p", "hint", data.completions.notice));
  }
  renderResourcePreview(data) {
    const result = data?.resource, value = result?.value;
    const key = `${data?.generation}:${data?.resource_revision}`;
    if (key === this.resourcePreviewKey) return;
    this.resourcePreviewKey = key;
    this.resourcePreview.replaceChildren(); this.resourcePreview.hidden = value?.kind !== "read";
    if (value?.kind !== "read") return;
    const title = element("div", "resource-preview-title");
    title.append(element("strong", "", value.resource.name), button("Close preview", async () => { await this.action("resource_close"); this.input.focus(); }, "quiet"));
    this.resourcePreview.append(title, element("p", "hint", value.resource.source), element("pre", "resource-preview-text", value.text));
  }
  renderCommands(data) {
    this.renderCompletions(data);
    this.renderResourcePreview(data);
    this.commands.disabled = !data || this.switching;
    const key = JSON.stringify([data?.generation, data?.commands, data?.command_receipt]);
    if (key === this.commandKey) return;
    this.commandKey = key;
    this.commandView.replaceChildren();
    for (const item of data?.commands?.commands ?? []) {
      const select = button(`/${item.name}`, () => {
        this.input.value = `/${item.name} `;
        this.edit(this.input.value); this.input.focus();
      }, "quiet");
      select.title = item.description;
      select.disabled = data.commands.revision.kind === "draft" && !item.draft_safe;
      this.commandView.append(select, element("span", "command-description", item.description));
    }
    const receipt = data?.command_receipt;
    if (receipt) {
      const result = receipt.outcome;
      this.commandView.append(element("p", "command-receipt", `${receipt.command} · ${result.kind === "draft_changed" ? `Draft changed · revision ${result.revision}` : `Committed · control ${result.control_seq}`} · ${receipt.request_id}`));
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
    this.historyContext = data ? {session:data.session,path:data.path} : undefined;
    const changed = this.generation !== data?.generation;
    if (this.selection !== data?.selection) { this.selection = data?.selection; this.switching = false; }
    if (changed) {
      this.referenceDialog?.close();
      this.fileDialog?.close();
      this.switching = false;
      this.exportAbort?.abort();
      this.generation = data?.generation;
      this.completionState = undefined; this.completionRequestKey = undefined;
      this.completionFailure = undefined; this.completionPanel.hidden = true;
      clearTimeout(this.completionTimer);
      this.binding = this.bindDraft(data).catch(error => notify(error.message));
      this.blocks.clear(); this.transcript.replaceChildren();
      this.pendingKey = undefined;
      this.visibleKey = undefined;
    }
    this.historical = !!data?.historical;
    this.uiCards = !!data?.ui_cards;
    this.uiRevision = data?.ui_revision;
    const uiKey = JSON.stringify([data?.generation, data?.ui_surfaces, view?.has_remote_ui]);
    if (uiKey !== this.uiKey) {
      this.uiKey = uiKey;
      this.uiMenu.replaceChildren(...(data?.ui_surfaces ?? []).map(surface => button(surface.title,
        () => this.action("ui_surface", { reference: surface.reference }), "quiet")));
      if (data && view?.has_remote_ui) this.uiMenu.append(button("Service extensions", () => this.action("remote_ui_list"), "quiet"));
    }
    this.name.textContent = data ? basename(data.path) : "New conversation";
    this.session.textContent = data ? `${data.path} · ${data.session}` : "Select a workspace to begin";
    this.session.title = this.session.textContent;
    this.status.textContent = data?.historical ? "History" : (data?.transcript.status || "Ready");
    this.history.disabled = !data || (!data.history_more && data.historical);
    this.live.hidden = !data?.historical;
    const modelPending = !!data?.model_command?.pending;
    this.model.disabled = !data || this.switching || modelPending;
    this.modelReceipt.hidden = !modelPending;
    this.effort.hidden = !data?.effort_profile?.supported?.length;
    this.effort.disabled = !data || this.switching || modelPending;
    this.cancel.disabled = !data;
    this.renderCommands(data);
    this.renderImages(data);
    this.renderExtensions(data);
    this.renderComposer();
    if (!data) {
      if (!this.transcript.querySelector(".empty-pane")) {
        const empty = element("div", "empty-pane");
        const glyph = element("div", "empty-glyph"); glyph.setAttribute("aria-hidden", "true"); glyph.append(element("span"), element("span"));
        empty.append(glyph, element("h3", "", this.index === "compare" ? "Compare another conversation." : "Start a conversation."),
          element("p", "", "Choose a workspace or reopen a conversation from the sidebar."));
        this.transcript.replaceChildren(empty);
      }
      this.waiting.replaceChildren(); this.pendingKey = undefined; this.notice.textContent = ""; return;
    }
    const allModels = models.some(model => sameModel(model, data.model)) ? models : [data.model, ...models];
    const modelKey = JSON.stringify(allModels);
    if (modelKey !== this.modelKey) {
      this.modelKey = modelKey;
      this.model.replaceChildren(...allModels.map(model => {
        const option = element("option", "", `${model.model} · ${model.deployment}`); option.value = JSON.stringify(model); return option;
      }));
    }
    this.model.value = JSON.stringify(data.model);
    const effortKey = JSON.stringify([data.effort_profile, data.reasoning_effort]);
    if (effortKey !== this.effortKey) {
      this.effortKey = effortKey;
      const profile = data.effort_profile;
      const defaultChoice = element("option", "", profile?.default ? `Default (${profile.default})` : "Provider default");
      defaultChoice.value = "";
      this.effort.replaceChildren(defaultChoice, ...(profile?.supported ?? []).map(value => {
        const option = element("option", "", value); option.value = value; return option;
      }));
      this.effort.value = data.reasoning_effort ?? "";
    }
    this.renderTranscript(data.transcript, changed);
    this.renderInline(data.inline);
    this.scheduleVisible();
    const pendingKey = JSON.stringify(data.pending);
    if (pendingKey !== this.pendingKey) {
      this.pendingKey = pendingKey;
      this.waiting.replaceChildren(...data.pending.map(item => button(`${item.kind === "approval" ? "Review" : "Answer"}: ${item.title}`,
        () => this.action("inspect_interaction", { owner: item.owner, id: item.id }))));
    }
    this.notice.textContent = data.notice;
  }
  renderTranscript(transcript, changed) {
    const previousHeight = this.transcript.scrollHeight;
    const atEnd = previousHeight - this.transcript.clientHeight - this.transcript.scrollTop < 70;
    const keys = new Set(transcript.blocks.map(block => block.key));
    for (const [key, entry] of this.blocks) if (!keys.has(key)) { entry.node.remove(); this.blocks.delete(key); }
    if (this.transcript.querySelector(".empty-pane")) this.transcript.replaceChildren();
    let omitted = this.transcript.querySelector(":scope > .omitted");
    if (transcript.omitted && !omitted) {
      omitted = element("p", "omitted", "Older content was omitted from this view. Open history to read earlier facts.");
      this.transcript.prepend(omitted);
    }
    if (!transcript.omitted) { omitted?.remove(); omitted = undefined; }
    let previous = omitted;
    for (const block of transcript.blocks) {
      let entry = this.blocks.get(block.key);
      if (!entry) {
        const node = element("article", `message ${block.role}`);
        const title = element("p", "message-title"); const text = element("div", "message-text"); const clipped = element("p", "omitted", "Text shortened in this view.");
        const sources = element("div", "actions source-actions");
        const inline = element("div", "inline-card"); inline.hidden = true;
        node.append(title, text, clipped, inline, sources); entry = { node, title, text, clipped, inline, sources }; this.blocks.set(block.key, entry);
      }
      if (entry.title.textContent !== block.title) entry.title.textContent = block.title;
      if (entry.source !== block.text || entry.markdown !== Boolean(block.markdown)) {
        entry.source = block.text; entry.markdown = Boolean(block.markdown);
        entry.text.classList.toggle("markdown", entry.markdown);
        if (block.markdown) entry.text.replaceChildren(markdown(block.markdown));
        else entry.text.textContent = block.text;
      }
      entry.node.classList.toggle("delegation", Boolean(block.external));
      entry.clipped.hidden = !block.clipped;
      const sourceKey = JSON.stringify([block.tool, block.external, block.sources, this.uiCards]);
      if (sourceKey !== entry.sourceKey) {
        entry.sourceKey = sourceKey;
        entry.sources.replaceChildren();
        if (this.uiCards) entry.sources.append(button("Card details", () => this.action("ui_block", { key: block.key }), "quiet"));
        if (block.external) entry.sources.append(button("Open external conversation", () => this.action("delegation_open", { key: block.key }), "quiet"));
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
    if (changed || (atEnd && this.transcript.scrollHeight !== previousHeight)) this.transcript.scrollTop = this.transcript.scrollHeight;
  }
}
function basename(path) { return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path; }
function sameModel(left, right) { return left.deployment === right.deployment && left.model === right.model; }
const panes = new Map();
const ExternalPane = externalPaneClass({element,button,command,perform});
function select(index) {
  selected = index;
  for (const [key, pane] of panes) { pane.node.classList.toggle("selected", key === index); pane.node.hidden = key !== index; }
  selectSurface(index);
}
async function openInSelected(fields) {
  const pane = panes.get(selected);
  if (!pane) throw new Error("Open a conversation surface first");
  if (pane.switching) throw new Error("This pane is still opening a conversation");
  pane.switching = true;
  for (const control of [pane.input,pane.model,pane.send,pane.steer]) if(control)control.disabled=true;
  try {
    await pane.flush().catch(() => {});
    const editor = pane.editor;
    const reuse = fields.action === "create" && editor && !editor.dirty && !editor.failure && !editor.record.pending && !editor.text && !editor.images.length && !editor.references.length && !pane.submitting && !pane.uploading
      ? {generation:pane.generation,header:editor.record.header} : undefined;
    await command({ ...fields, pane: pane.index, ...(reuse ? {reuse} : {}) });
  } catch (error) {
    pane.switching = false;
    pane.render(view?.surfaces[pane.index], view?.catalog.models ?? []);
    throw error;
  }
}
function render(next) {
  rendererSlots = [];
  view = next;
  if (next.notice !== lastNotice) { lastNotice = next.notice; notify(next.notice); }
  for (const [key, pane] of panes) if (!Object.hasOwn(next.surfaces, key) || (pane.kind === "external") !== (next.surfaces[key]?.kind === "external")) { pane.resizeObserver?.disconnect(); if (pane.visibleFrame) cancelAnimationFrame(pane.visibleFrame); pane.reset(); pane.node.remove(); panes.delete(key); }
  for (const [key, data] of Object.entries(next.surfaces)) {
    if (!panes.has(key)) panes.set(key, data?.kind === "external" ? new ExternalPane(key) : new Pane(key));
    const pane = panes.get(key);
    pane.enterSubmit = next.preferences?.enter_submit ?? false;
    pane.hint.textContent = pane.enterSubmit ? "Enter to send · Shift Enter for a new line" : "Ctrl / ⌘ Enter to send · Enter for a new line";
    pane.render(data, next.catalog.models);
  }
  if (!panes.has(selected)) selected = panes.keys().next().value;
  select(selected);
  publish(next);
  renderDetail(next);
}
installActions({ command, open: openInSelected, select, call,
  async closeSurface(key) {
    const pane = panes.get(key);
    if (pane?.switching || pane?.submitting || pane?.uploading) throw new Error("Wait for this conversation's current operation before closing");
    if (pane) { pane.switching = true; pane.renderComposer(); }
    try { if (pane) await pane.flush(true); await command({action:"close_surface",pane:key}); }
    catch (error) { if (pane) { pane.switching = false; pane.renderComposer(); } throw error; }
  },
  async addSurface(key) { await command({action:"add_surface",pane:key}); select(key); },
});

function showDialog(key, title, body) {
  if (dialogKey === key) return;
  dialogKey = key; $("detail-title").textContent = title; $("detail-body").replaceChildren(body);
  if (!$("detail").open) $("detail").showModal();
}
async function closeDetail() { await command({ action: "close_detail" }); dialogKey = undefined; $("detail").close(); }
$("detail-close").addEventListener("click", () => perform(closeDetail));
$("detail").addEventListener("cancel", event => { event.preventDefault(); perform(closeDetail); });
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
  if (next.remote_ui_catalog) {
    const catalog = next.remote_ui_catalog;
    const key = JSON.stringify(catalog);
    if (dialogKey === key) return;
    const body = element("div", "settings-list");
    if (catalog.error) body.append(element("p", "settings-error", catalog.error));
    else if (!catalog.page) body.append(element("p", "", "Loading service extensions…"));
    else {
      for (const entry of catalog.page.entries) body.append(button(entry.title, () => command({ action: "remote_ui_surface", ticket: catalog.ticket, bundle: entry.bundle, surface: entry.surface }), "quiet"));
      if (!catalog.page.entries.length) body.append(element("p", "", "No service extensions."));
      if (catalog.page.next) body.append(button("More extensions", () => command({ action: "remote_ui_next", ticket: catalog.ticket }), "quiet"));
    }
    showDialog(key, "Service extensions", body); return;
  }
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
    const safeForm = canUseSettingsForm(editor.text);
    const fields = safeForm ? settingsForm(description.metadata.schema, JSON.parse(editor.text), !description.writable) : {element:element("p","hint","This value contains exact numeric text; use JSON to preserve it."),read:()=>{throw new Error("Use the exact JSON editor")}};
    let jsonMode = !safeForm;
    const toggle = button("Edit as JSON", () => {
      if (!jsonMode) {text.value=fields.json();failure.textContent="";jsonMode=true;fields.element.hidden=true;text.hidden=false;toggle.hidden=true;text.focus()}
    });
    text.hidden=safeForm;toggle.hidden=!safeForm;
    const failure = element("p", "settings-error settings-field-error");failure.setAttribute("role","alert");
    form.append(element("p", "hint", "Saving requires the version you opened."), fields.element, toggle, text, failure, save);
    if (!description.writable) { text.readOnly = true; save.disabled = true; form.append(element("p", "hint", "Settings provider is read-only.")); }
    if (new TextEncoder().encode(editor.text).length > 1024 * 1024) {
      fields.element.disabled=true;text.readOnly=true;save.disabled=true;form.append(element("p", "hint", "This value exceeds the Web editor's 1 MiB input limit."));
    }
    form.addEventListener("submit", async event => {
      event.preventDefault();if(save.disabled)return;save.disabled=true;failure.textContent="";
      try {await command({action:"settings_save",ticket:editor.ticket,text:jsonMode ? text.value : JSON.stringify(fields.read())})}
      catch(error) {failure.textContent=error instanceof Error ? error.message : String(error);save.disabled=!description.writable}
    });
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
  if (!detail.model || !detail.binding) {
    const body = element("p", "hint", detail.error ?? "Loading…");
    showDialog(`ui-loading:${detail.ticket}:${detail.error ?? ""}`, "Card details", body);
    return;
  }
  const binding = JSON.stringify(detail.binding);
  const key = `ui:${binding}`;
  let body;
  if (dialogKey === key) body = $("detail-body").firstElementChild;
  else { body = element("div", "ui-presentation"); showDialog(key, detail.model.standard_view?.title ?? "Card details", body); }
  rendererSlots.push({ key: "detail", surface: "dialog", root: body, binding,
    snapshot: { model: detail.model, busy: detail.busy, error: detail.error },
    host: { invoke(action, input) {
      if (detail.busy || !detail.model.actions.some(item => item.name === action)) throw new Error("This action is no longer available");
      return command({ action: "ui_invoke", ticket: detail.ticket, name: action, input });
    }, source(name, offset, maximum) { return call("ui_source", JSON.stringify({ ticket: detail.ticket, name, offset, maximum })); } }
  });
}

if (location.protocol === "rsi:") {
  $("login").hidden = true;
  $("sign-out").textContent = "Close application";
  perform(() => connectWith("{}"));
}
}
