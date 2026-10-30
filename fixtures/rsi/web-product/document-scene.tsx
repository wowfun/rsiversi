import { mountWorkbench } from "./src/workbench-root.tsx";
import { DraftStore, DraftEditor } from "./drafts.js";
import { MountTable } from "./mounts.js";
import { slots } from "./src/slots.tsx";
import { useView, input } from "./src/bridge.ts";
const calls = [],
  failures = [];
const reference = {
  snapshot: {
    capture_id: "fixture-capture",
    sha256: "a".repeat(64),
    byte_len: 900,
  },
  preview: "Reference",
  metadata: {
    source: {
      kind: "native",
      binding: { session_id: "source", header_sha256: "b".repeat(64) },
    },
    target: { session_id: "fixture-session", header_sha256: "c".repeat(64) },
    capture: {
      kind: "suffix",
      interval: {
        through_seq: "1",
        scanned_after_seq: "0",
        retained_after_seq: "0",
        retained_through_seq: "1",
        fact_prefix_sha256: "d".repeat(64),
        scanned_bytes: 100,
        omissions: [],
      },
    },
    text_bytes: 9,
  },
};
let release,
  hold = false,
  rendererError = false,
  detached = false,
  probeInline = false,
  inlineRoot,
  retainedInline;
const table = {
  async render(_assets, slots) {
    if (probeInline) {
      if (slots.length) {
        inlineRoot = slots[0].root;
        const child = document.createElement("span");
        child.textContent = "Owned renderer";
        inlineRoot.append(child);
      } else if (inlineRoot) {
        retainedInline =
          inlineRoot.textContent === "Owned renderer" &&
          inlineRoot.firstChild?.isConnected;
        inlineRoot.replaceChildren();
      }
    }
    if (hold)
      await new Promise((resolve) => {
        release = resolve;
      });
    if (detached) document.querySelector(".pane")?.remove();
    if (rendererError === "throw") throw new Error("fixture renderer failed");
    return rendererError ? { revision: "rejected", accept: false, error: "fixture renderer failed" } : {};
  },
  close() {},
};
const replies = new Map(),
  transports = [],
  nativeCalls = [];
let transport, nativeHandler;
function defaultReply(message) {
  return message.method === "connect"
    ? JSON.stringify({
        endpoint_id: "a".repeat(32),
        principal: { kind: "local" },
      })
    : message.method === "file_input"
      ? JSON.stringify(
          JSON.parse(message.payload).request?.file_kind === "directory"
            ? {
                kind: "directory",
                path: "",
                revision: "directory-1",
                offset: "0",
                next_offset: "1",
                total: "1",
                more: false,
                entries: [
                  { name: "test.txt", path: "746573742e747874", kind: "file" },
                ],
              }
            : {
                kind: "file",
                path: "746573742e747874",
                revision: "file-1",
                offset: "0",
                next_offset: "4",
                total: "4",
                more: false,
                text: "Text",
                locator: "@test.txt",
              },
        )
      : message.method === "reference_input"
        ? JSON.stringify(
            JSON.parse(message.payload).operation.kind === "capture"
              ? reference
              : {},
          )
        : message.method === "prepare_submission"
          ? JSON.stringify({
              content: [
                { type: "text", text: "Queued original" },
                { type: "image", media: { fixture: "retained" } },
              ],
            })
          : message.method === "terminal"
            ? '{"value":[]}'
            : "{}";
}
function makeTransport() {
  const next = {
    calls: [],
    terminated: 0,
    terminate() {
      this.terminated++;
      return this.teardown?.();
    },
    postMessage(message) {
      calls.push(message);
      this.calls.push(message);
      if (message.kind === "call")
        queueMicrotask(async () => {
          try {
            const handler = replies.get(message.method);
            const result = await (handler
              ? handler(message.payload, message)
              : defaultReply(message));
            await this.onmessage?.({
              data: { kind: "reply", id: message.id, result },
            });
          } catch (error) {
            await this.onmessage?.({
              data: {
                kind: "reply",
                id: message.id,
                error: String(error.message ?? error),
                notAdmitted: error.notAdmitted,
                retryable: error.retryable,
              },
            });
          }
        });
    },
  };
  transports.push(next);
  transport = next;
  return next;
}
let factoryTable = table;
makeTransport();
const owner = mountWorkbench(document.getElementById("root"), {
  nativeDocument: new URL(location.href).searchParams.has("native"),
  nativeCall: async (value) => {
    nativeCalls.push(value);
    return nativeHandler?.(value);
  },
  transportFactory: makeTransport,
  mountTable: { open: async () => factoryTable },
});
const runtime = owner.runtime;
await runtime.attach(transport, table, {}, async () => ({
  drafts: await DraftStore.open("a".repeat(32), { kind: "local" }),
}));
const model = { deployment: "fixture", model: "deterministic" };
const workspace = {
  id: "fixture",
  coordinates: { location: { kind: "local" }, path: "/fixture" },
};
const view = {
  notice: "",
  surfaces: {
    main: {
      generation: "1",
      selection: "1",
      session: "fixture-session",
      header: "c".repeat(64),
      creation: null,
      path: "/fixture",
      workspace: "fixture",
      model,
      transcript: {
        blocks: [
          {
            key: "answer",
            role: "assistant",
            title: "Assistant",
            text: "Frame one",
            sources: 0,
          },
        ],
        status: "Ready",
        omitted: false,
      },
      pending: [],
      notice: "",
      history_more: false,
      historical: false,
      ui_surfaces: [],
      block_actions: [],
      extension: {},
    },
  },
  setup: {
    ticket: "fixture",
    allowed: false,
    credentials_available: false,
    providers_writable: false,
    settings_writable: false,
    agent: { default_model: model },
    presets: null,
    providers: null,
    diagnostic: null,
    receipts: [],
  },
  catalog: {
    workspaces: [workspace],
    workspaces_more: false,
    workspace_order_seed: { kind: "available", records: [workspace] },
    models: [model],
    models_more: false,
  },
  preferences: { submit_key: "mod_enter" },
  composer_actions: {
    main: { revision: "1", primary: { id: "queue", label: "Send" } },
  },
  application_surfaces: [],
  has_remote_ui: false,
};
const nativeSurface = structuredClone(view.surfaces.main);
let id = 0;
window.fixture = {
  runtime,
  owner,
  calls,
  view,
  DraftStore,
  DraftEditor,
  MountTable,
  transports,
  nativeCalls,
  makeTransport,
  input,
  assets: {},
  setReply(method, handler) {
    if (handler) replies.set(method, handler);
    else replies.delete(method);
  },
  nativeReply(handler) {
    nativeHandler = handler;
  },
  table(value) {
    factoryTable = value;
  },
  get transport() {
    return transport;
  },

  async frame() {
    await transport.onmessage({
      data: {
        kind: "view",
        view: {
          kind: "snapshot",
          frame_id: String(++id),
          view: structuredClone(view),
        },
        assets: this.assets,
      },
    });
  },
  async surface(session, fields = {}) {
    view.surfaces.main = {
      ...structuredClone(nativeSurface),
      session,
      generation: session,
      selection: session,
      ...fields,
    };
    await this.frame();
  },
  async tick() {
    await new Promise((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(resolve)),
    );
  },
  hold() {
    hold = true;
  },
  release() {
    hold = false;
    release?.();
  },
  rendererError(throwError = true) {
    rendererError = throwError ? "throw" : "reject";
  },
  detach() {
    detached = true;
  },
  breakSlot() {
    slots.register(
      { name: "rsi.navigation", registrant: "fixture.failure", priority: -100 },
      () => {
        useView((v) => v);
        throw new Error("fixture React slot failed");
      },
    );
  },
  probeInline() {
    probeInline = true;
  },
  get retainedInline() {
    return retainedInline;
  },
  get acknowledgements() {
    return calls.filter((call) => call.kind === "ack");
  },
};
window.ready = true;
