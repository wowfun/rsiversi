// Presentation resource ownership only. Domain authority remains in the Worker.
const encoder = new TextEncoder();
// ESM records outlive a Worker/MountTable, so admission belongs to the document.
const importedGenerations = new Set();
let cleanupBlocked = false;
const surfaces = new Set(["root", "pane", "sidebar", "dialog"]);
const digest = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const name = value => typeof value === "string" && /^[A-Za-z0-9_.-]{1,128}$/.test(value);
function bounded(value, maximum) {
  if (encoder.encode(JSON.stringify(value)).length > maximum) throw new Error("Presentation exceeds its byte limit");
}
function model(snapshot) {
  const value = snapshot.model;
  bounded(value, 128 * 1024);
  if (!name(value?.renderer) || !name(value?.schema?.name) || !Number.isInteger(value.schema.version) || value.schema.version < 1) throw new Error("Invalid renderer model");
  for (const key of ["actions", "sources"]) {
    if (!Array.isArray(value[key]) || value[key].length > 32 || value[key].some(item => !name(item.name)) || new Set(value[key].map(item => item.name)).size !== value[key].length) throw new Error("Invalid model membership");
  }
  return value;
}
function select(offer, slot, required) {
  const value = model(slot.snapshot);
  if (offer.catalog === null) return null;
  const renderer = offer.catalog?.renderers.find(renderer => renderer.id === value.renderer);
  if (!renderer || renderer.abi !== 1 || !renderer.surfaces.includes(slot.surface) || !renderer.schemas.some(schema => schema.name === value.schema.name && schema.version === value.schema.version)) {
    if (!required) return null;
    throw new Error(`Renderer unavailable: ${value.renderer} (${value.schema.name} v${value.schema.version})`);
  }
  if (!name(renderer.entry) || renderer.entry.startsWith(".") || !renderer.entry.endsWith(".js") || !renderer.files.some(file => file.name === renderer.entry && digest(file.sha256))) throw new Error("Invalid renderer entry");
  return renderer;
}
async function drain(entry) {
  entry.active = false;
  entry.abort.abort();
  entry.cleanup ??= (async () => {
    try { await entry.mounted?.dispose(); }
    finally { await Promise.allSettled([...entry.work]); }
  })();
  await entry.cleanup;
}
function binding(slot, renderer, input) {
  const entry = { slot, renderer, input, active: false, abort: new AbortController(), root: document.createElement("div"), work: new Set() };
  entry.root.className = "renderer-mount";
  const live = () => { if (!entry.active || entry.abort.signal.aborted) throw new Error("Presentation binding retired"); };
  const run = (kind, action, args) => {
    live();
    if (!renderer.capabilities.includes(kind)) throw new Error(`Renderer has no ${kind} capability`);
    if (!model(entry.slot.snapshot)[kind === "invoke" ? "actions" : "sources"].some(item => item.name === action)) throw new Error("Action or source is absent from displayed model");
    if (entry.work.size >= 8) throw new Error("Presentation input is busy");
    bounded(args, 64 * 1024);
    const slot = entry.slot;
    const promise = Promise.resolve().then(() => {
      if (entry.abort.signal.aborted) throw new Error("Presentation binding retired");
      return slot.host[kind](action, ...args, entry.abort.signal);
    });
    entry.work.add(promise);
    promise.then(() => entry.work.delete(promise), () => entry.work.delete(promise));
    return promise;
  };
  const host = {
    invoke: (action, input) => run("invoke", action, [input]),
    source: async (source, offset, maximum) => {
      if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(maximum) || maximum < 1 || maximum > 64 * 1024) throw new Error("Invalid source window");
      const result = await run("source", source, [offset, maximum]);
      if (!(result instanceof Uint8Array) || result.byteLength > maximum) throw new Error("Source exceeded its window");
      return result;
    },
    focus: node => { live(); if (!renderer.capabilities.includes("focus") || !entry.root.contains(node)) throw new Error("Focus is outside this presentation"); node.focus(); },
    clipboard: async text => { live(); if (!renderer.capabilities.includes("clipboard") || typeof text !== "string" || encoder.encode(text).length > 64 * 1024) throw new Error("Clipboard capability unavailable"); await navigator.clipboard.writeText(text); },
    input: key => input.get(key),
    setInput: (key, value) => {
      live();
      if (!name(key) || typeof value !== "string" || (!input.has(key) && input.size >= 32)) throw new Error("Invalid local input");
      const previous = input.get(key); input.set(key, value);
      try { bounded(Object.fromEntries(input), 64 * 1024); }
      catch (error) { if (previous === undefined) input.delete(key); else input.set(key, previous); throw error; }
    },
  };
  entry.host = Object.freeze(host);
  return entry;
}
export class MountTable {
  constructor() { this.entries = new Map(); this.offer = undefined; this.rejected = undefined; this.busy = false; this.closed = false; }
  async render(offer, slots) {
    if (cleanupBlocked) throw new Error("Renderer cleanup incomplete; reload the page");
    if (this.closed || this.busy) throw new Error("Presentation mount table is unavailable");
    if (!digest(offer?.revision) || slots.length > 16 || new Set(slots.map(slot => slot.key)).size !== slots.length || slots.some(slot => !name(slot.key) || !surfaces.has(slot.surface) || !(slot.root instanceof Element))) throw new Error("Invalid presentation slots");
    bounded(offer, 256 * 1024);
    this.busy = true;
    this.rendering = this.apply(offer, slots);
    try { return await this.rendering; }
    finally { this.busy = false; this.rendering = undefined; }
  }
  async apply(offer, slots) {
    const offered = offer.revision !== this.offer?.revision && offer.revision !== this.rejected;
    const selected = offered ? offer : this.offer;
    if (!selected) throw new Error("No renderer generation was accepted");
    const staged = new Map();
    this.staged = staged;
    let replacing = false;
    try {
      if (selected.catalog === null && this.offer?.catalog && slots.length) throw new Error("Renderer catalog unavailable");
      for (const slot of slots) {
        const renderer = select(selected, slot, offered || this.rejected === undefined);
        const old = this.entries.get(slot.key);
        if (!offered && old?.slot.binding === slot.binding && old.renderer?.id === renderer?.id) continue;
        const input = old?.slot.binding === slot.binding ? old.input : new Map();
        const entry = binding(slot, renderer ?? { capabilities: [] }, input);
        entry.renderer = renderer;
        staged.set(slot.key, entry);
        if (!renderer) {
          const update = snapshot => { entry.root.textContent = `Renderer unavailable: ${model(snapshot).renderer}`; };
          update(slot.snapshot);
          entry.mounted = { update, dispose() { entry.root.replaceChildren(); } };
          continue;
        }
        if (!importedGenerations.has(selected.revision)) {
          if (importedGenerations.size >= 32) throw new Error("Renderer generation limit reached; reload the page for further updates");
          importedGenerations.add(selected.revision);
        }
        const module = await import(`/rsi-renderers/${selected.revision}/${renderer.entry}`);
        if (this.closed) throw new Error("Presentation mounting stopped");
        if (typeof module.mount !== "function") throw new Error("Renderer exports no mount function");
        entry.mounted = await module.mount(entry.root, slot.snapshot, entry.host, entry.abort.signal);
        if (typeof entry.mounted?.update !== "function" || typeof entry.mounted?.dispose !== "function") throw new Error("Renderer returned an invalid lifecycle");
        if (this.closed) throw new Error("Presentation mounting stopped");
      }
      // Fence input while renderer-owned DOM and the host snapshot are changing.
      for (const slot of slots) {
        if (staged.has(slot.key)) continue;
        const entry = this.entries.get(slot.key);
        entry.active = false;
        await entry.mounted.update(slot.snapshot);
        if (this.closed) throw new Error("Presentation mounting stopped");
        entry.slot = slot;
        entry.active = true;
      }
      replacing = true;
      const retained = new Set(slots.map(slot => slot.key));
      for (const [key, entry] of this.entries) {
        if (staged.has(key) || !retained.has(key)) {
          entry.active = false;
          await drain(entry);
          if (!staged.has(key)) entry.root.remove();
          this.entries.delete(key);
        }
      }
      if (this.closed) throw new Error("Presentation mounting stopped");
      for (const [key, entry] of staged) {
        entry.slot.root.replaceChildren(entry.root);
        entry.active = true;
        this.entries.set(key, entry);
      }
      this.staged = undefined;
      if (offered && selected.catalog !== null && slots.length === 0) return undefined;
      this.offer = selected;
      if (offered) { this.rejected = undefined; return { revision: offer.revision, accept: true }; }
      return undefined;
    } catch (error) {
      const results = await Promise.allSettled([...staged.values()].map(drain));
      this.staged = undefined;
      if (results.some(result => result.status === "rejected")) {
        this.cleanupError = new Error("Candidate renderer disposal failed", { cause: error });
        throw this.cleanupError;
      }
      if (this.closed || !offered || replacing) throw error;
      this.rejected = offer.revision;
      this.offer ??= { revision: offer.revision, catalog: null };
      await this.apply(this.offer, slots);
      return { revision: offer.revision, accept: false, error: error.message };
    }
  }
  async close() {
    this.closed = true;
    for (const entry of [...this.entries.values(), ...(this.staged?.values() ?? [])]) {
      entry.active = false; entry.abort.abort();
    }
    this.closing ??= (async () => {
      await this.rendering?.catch(() => {});
      const entries = [...this.entries.values()];
      this.entries.clear();
      const results = await Promise.allSettled(entries.map(drain));
      for (const entry of entries) entry.root.remove();
      if (this.cleanupError || results.some(result => result.status === "rejected")) throw this.cleanupError ?? new Error("Renderer cleanup failed");
    })();
    this.closeResult ??= (async () => {
      let timer;
      try {
        await Promise.race([this.closing, new Promise((_, reject) => {
          timer = setTimeout(() => {
            cleanupBlocked = true;
            reject(new Error("Renderer cleanup exceeded 30 seconds; reload the page"));
          }, 30000);
        })]);
      } finally { clearTimeout(timer); }
    })();
    await this.closeResult;
  }
}
