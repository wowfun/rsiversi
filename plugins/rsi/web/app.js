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
let endpoint;
try { endpoint = localStorage.getItem("rsi.endpoint"); } catch { /* Storage is optional. */ }

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
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
function makeWorker() {
  const current = new Worker("/worker.js", { type: "module" });
  current.onmessage = ({ data }) => {
    if (worker !== current) return;
    if (data.kind === "view") {
      try { render(JSON.parse(data.view)); }
      catch (error) { failWorker(`View rendering failed: ${error.message}`); }
      finally { if (worker === current) current.postMessage({ kind: "ack" }); }
    } else if (data.kind === "reply") {
      const waiter = pending.get(data.id); pending.delete(data.id);
      if (data.error) waiter?.reject(new Error(data.error)); else waiter?.resolve(data.result);
    } else if (data.kind === "failed") { failWorker(data.error); }
  };
  current.onerror = event => { event.preventDefault(); if (worker === current) failWorker("Browser Worker stopped"); };
  return current;
}
function call(method, payload) {
  if (!worker) return Promise.reject(new Error("Connect to your service first"));
  if (pending.size >= 8) return Promise.reject(new Error("Input is busy; wait for the current action"));
  const id = ++requestId;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    worker.postMessage({ kind: "call", id, method, payload });
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
    tools.append(this.history, this.live, this.commands);
    this.commandView = element("div", "session-commands");
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
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") { event.preventDefault(); perform(() => this.submit(false)); }
    });
    this.composer.addEventListener("submit", event => { event.preventDefault(); perform(() => this.submit(false)); });
    const bar = element("div", "composer-bar");
    this.model = element("select"); this.model.setAttribute("aria-label", `${index ? "Right" : "Left"} model`);
    this.model.addEventListener("change", () => perform(() => this.action("model", { model: JSON.parse(this.model.value) })));
    const actions = element("div", "actions");
    this.cancel = button("Cancel", () => this.action("cancel"), "quiet");
    this.steer = button("Steer", () => this.submit(true));
    this.send = button("Send ↗", () => this.submit(false), "primary");
    actions.append(this.cancel, this.steer, this.send); bar.append(this.model, actions);
    this.composer.append(this.input, bar, element("div", "composer-hint", "Ctrl / ⌘ Enter to send · Enter for a new line"));
    this.node.append(header, tools, this.commandView, this.transcript, this.waiting, this.notice, this.composer);
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
    if (this.submitting) return;
    this.submitting = true; this.send.disabled = true; this.steer.disabled = true;
    try {
      await this.flush();
      const generation = this.generation;
      const text = this.input.value;
      const submittedText = this.retryText ?? text;
      await this.action("submit", { text, steer });
      if (this.generation === generation && this.input.value === text && text === submittedText) this.input.value = "";
    } finally { this.submitting = false; this.send.disabled = !this.generation; this.steer.disabled = !this.generation || this.retryText != null; }
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
    this.name.textContent = data ? basename(data.path) : "New conversation";
    this.session.textContent = data ? `${data.path} · ${data.session}` : "Select a workspace to begin";
    this.session.title = this.session.textContent;
    this.status.textContent = data?.historical ? "History" : (data?.transcript.status || "Ready");
    this.history.disabled = !data || (!data.history_more && data.historical);
    this.live.hidden = !data?.historical;
    this.input.disabled = !data || this.switching; this.model.disabled = !data || this.switching;
    this.send.disabled = !data || this.submitting || this.switching; this.steer.disabled = !data || this.submitting || this.switching;
    this.retryText = data?.unresolved_text;
    this.send.textContent = this.retryText != null ? "Retry previous" : "Send ↗";
    this.steer.disabled ||= this.retryText != null;
    this.cancel.disabled = !data;
    this.renderCommands(data);
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
        const title = element("p", "message-title"); const text = element("p", "message-text"); const clipped = element("p", "omitted", "Text shortened in this view.");
        node.append(title, text, clipped); entry = { node, title, text, clipped }; this.blocks.set(block.key, entry);
      }
      if (entry.title.textContent !== block.title) entry.title.textContent = block.title;
      if (entry.text.textContent !== block.text) entry.text.textContent = block.text;
      entry.clipped.hidden = !block.clipped;
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
  next.panes.forEach((data, i) => panes[i].render(data, next.catalog.models));
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
$("settings-open").addEventListener("click", () => {
  const form = element("form"); const input = element("input"); input.value = "rsi.agent"; input.required = true; input.setAttribute("aria-label", "Settings namespace");
  const submit = element("button", "primary", "Read settings"); submit.type = "submit";
  form.append(element("label", "", "Settings namespace"), input, submit);
  form.addEventListener("submit", event => { event.preventDefault(); perform(() => command({ action: "settings_read", namespace: input.value })); });
  showDialog("settings-prompt", "Settings", form);
});
function renderDetail(next) {
  if (next.settings) {
    const editor = next.settings;
    if (dialogKey === editor.ticket) return;
    const form = element("form"); const text = element("textarea", "settings-text"); text.value = editor.text; text.spellcheck = false; text.setAttribute("aria-label", "Settings JSON");
    const save = element("button", "primary", "Save settings"); save.type = "submit";
    form.append(element("p", "hint", "Changes apply to new conversations. Saving requires the version you opened."), text, save);
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
