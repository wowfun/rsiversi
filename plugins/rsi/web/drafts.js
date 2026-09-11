const MiB = 1024 * 1024;
const encoder = new TextEncoder();
const bytes = value => encoder.encode(value).length;
const hex = (value, length = 64) => typeof value === "string" && value.length === length && /^[0-9a-f]+$/.test(value);
const integer = (value, maximum = Number.MAX_SAFE_INTEGER) => Number.isSafeInteger(value) && value >= 0 && value <= maximum;
const identity = value => typeof value === "string" && bytes(value) > 0 && bytes(value) <= 256 && !/[\s\u0000-\u001f\u007f]/u.test(value);
const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const bump = value => { if (!integer(value) || value === Number.MAX_SAFE_INTEGER) throw new Error("Draft revision exhausted"); return value + 1; };
const clone = value => structuredClone(value);
function require(condition, message = "Saved draft is invalid; it was left unchanged") { if (!condition) throw new Error(message); }

export function validateEditor(text, images) {
  require(typeof text === "string", "Draft text must be a string");
  const textBytes = bytes(text);
  require(textBytes <= MiB, "Draft exceeds 1 MiB of UTF-8 text");
  require(Array.isArray(images) && images.length <= 8, "A draft can hold at most eight images");
  for (const image of images) {
    require(image && Object.keys(image).sort().join() === "bytes,height,id,mime,width" && hex(image.id) && image.mime === "image/png" &&
      integer(image.bytes, 32 * MiB) && image.bytes > 0 && integer(image.width, 65535) && image.width > 0 &&
      integer(image.height, 65535) && image.height > 0 && image.width * image.height <= 100_000_000, "Saved image reference is invalid");
  }
  return textBytes;
}
function validatePending(pending) {
  require(pending && Object.keys(pending).sort().join() === "editRevision,id,images,kind,opaque,phase,text_bytes");
  require(pending && ["message", "command"].includes(pending.kind) && identity(pending.id));
  require(typeof pending.opaque === "string" && bytes(pending.opaque) <= (pending.kind === "message" ? 8 * MiB : 32 * 1024));
  require(integer(pending.text_bytes, MiB) && integer(pending.images, 8));
  require(["prepared", "dispatching", "unknown"].includes(pending.phase) && integer(pending.editRevision));
}
const surface = value => typeof value === "string" && /^[a-z0-9_-]{1,32}$/.test(value);
const principalKey = value => value === "local" || (typeof value === "string" && /^device:[0-9a-f]{32}$/.test(value));
function validateRecord(record, legacy = false) {
  require(record?.version === (legacy ? 1 : 2) && Array.isArray(record.key) && record.key.length === 4 &&
    hex(record.key[0], 32) && (legacy ? hex(record.key[1], 32) && integer(record.key[2], 1) : principalKey(record.key[1]) && surface(record.key[2])) && identity(record.key[3]));
  require(equal(record.scope, record.key.slice(0, 3)) && hex(record.header) && hex(record.incarnation, 32));
  require(integer(record.editRevision) && integer(record.pendingRevision) && typeof record.everDispatched === "boolean");
  validateEditor(record.text, record.images);
  if (record.pending !== null) {
    validatePending(record.pending);
    require(record.pending.editRevision <= record.editRevision && (record.pending.phase === "prepared" || record.everDispatched));
  }
  if (record.receipt !== null) require(typeof record.receipt === "string" && bytes(record.receipt) <= 4096);
  if (record.creation !== null) {
    const creation = record.creation;
    // The Rust Web README owns the default-preset Fresh creation contract.
    require(creation && Object.keys(creation).sort().join() === "agent_preset_id,session_id,workspace_id,workspace_trust");
    require(hex(creation.workspace_id) && creation.session_id === record.key[3] && creation.agent_preset_id === null &&
      ["trusted", "untrusted"].includes(creation.workspace_trust));
  }
  require(Object.keys(record).sort().join() === "creation,editRevision,everDispatched,header,images,incarnation,key,pending,pendingRevision,receipt,scope,text,version");
  return record;
}
const usage = record => record ? [1, bytes(record.text), record.pending?.text_bytes ?? 0, bytes(record.pending?.opaque ?? "")] : [0, 0, 0, 0];
const legacyLimits = [64, 2 * MiB, 2 * MiB, 16 * MiB];
const limits = legacyLimits.map(value => value * 2);
function validateCounters(value) {
  require(Array.isArray(value) && value.length === limits.length && value.every((count, index) => integer(count, limits[index])));
  return value;
}
function conflict(saved) {
  return Object.assign(new Error("This draft changed in another tab. Choose the saved input or replace only its text and images."), { code: "conflict", saved });
}
function sameIncarnation(expected, saved) {
  if (!saved || saved.incarnation !== expected.incarnation) throw conflict(saved);
}
function samePending(expected, saved) {
  sameIncarnation(expected, saved);
  if (saved.pendingRevision !== expected.pendingRevision || !equal(saved.pending, expected.pending)) throw conflict(saved);
}

function migrate(tx, fail) {
  const rows = [], totals = [[0,0,0,0], [0,0,0,0]], saved = [[0,0,0,0], [0,0,0,0]];
  let finished = 0;
  const complete = () => {
    if (++finished !== 2) return;
    try {
      require(equal(totals, saved), "Old draft counters are inconsistent; migration was aborted");
      const drafts = tx.objectStore("drafts"), usageStore = tx.objectStore("usage");
      drafts.clear(); usageStore.clear();
      for (const record of rows) {
        record.version = 2; record.key[1] = `device:${record.key[1]}`; record.key[2] = record.key[2] === 0 ? "main" : "compare";
        record.scope = record.key.slice(0,3); validateRecord(record); drafts.add(record);
      }
      usageStore.put(totals[0].map((count,index) => count + totals[1][index]), "aggregate");
    } catch (error) { fail(error); }
  };
  const records = tx.objectStore("drafts").openCursor();
  records.onsuccess = () => {
    try {
      const cursor = records.result; if (!cursor) { complete(); return; }
      const record = validateRecord(cursor.value, true); require(equal(cursor.key, record.key));
      const total = totals[record.key[2]];
      usage(record).forEach((cost,index) => total[index] += cost);
      require(total.every((count,index) => integer(count,legacyLimits[index])), "Old drafts exceed their recorded quota; migration was aborted");
      rows.push(record); cursor.continue();
    } catch (error) { fail(error); }
  };
  const counters = tx.objectStore("usage").openCursor();
  counters.onsuccess = () => {
    try {
      const cursor = counters.result; if (!cursor) { complete(); return; }
      require(integer(cursor.key,1));
      require(Array.isArray(cursor.value) && cursor.value.length === 4 && cursor.value.every((count,index) => integer(count,legacyLimits[index])));
      saved[cursor.key] = cursor.value; cursor.continue();
    } catch (error) { fail(error); }
  };
}
let database;
async function openDatabase() {
  if (!database) database = new Promise((resolve, reject) => {
    let failure;
    const request = indexedDB.open("rsi.composer", 2);
    request.onupgradeneeded = event => {
      const fail = error => { failure = error; request.transaction.abort(); };
      if (failure) { request.transaction.abort(); return; }
      if (event.oldVersion === 0) {
        const records = request.result.createObjectStore("drafts", { keyPath: "key" });
        records.createIndex("scope", "scope"); request.result.createObjectStore("usage");
      } else if (event.oldVersion === 1) migrate(request.transaction, fail);
      else fail(new Error("Unsupported saved draft schema"));
    };
    request.onerror = () => reject(failure ?? request.error);
    request.onblocked = () => { failure = new Error("Draft storage is waiting for another tab to close"); reject(failure); };
    request.onsuccess = () => {
      const db = request.result;
      if (failure) { db.close(); return; }
      db.onversionchange = () => { db.close(); database = undefined; };
      resolve(db);
    };
  }).catch(error => { database = undefined; throw error; });
  return database;
}

export class DraftStore {
  static async open(endpoint, principal) {
    require(hex(endpoint,32) && principal && (principal.kind === "local" ? Object.keys(principal).length === 1 : principal.kind === "device" && Object.keys(principal).length === 2 && hex(principal.device_id,32)), "Authenticated draft namespace is unavailable");
    const key = principal.kind === "local" ? "local" : `device:${principal.device_id}`;
    const store = new DraftStore(await openDatabase(), endpoint, key);
    await store.verify(); return store;
  }
  constructor(db, endpoint, principal) { this.database = db; this.namespace = [endpoint, principal]; }
  get db() { require(this.database, "Draft storage is unavailable. Input cannot be saved or sent."); return this.database; }
  key(pane, session) { require(surface(pane) && identity(session)); return [...this.namespace, pane, session]; }
  async verify() {
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction(["drafts","usage"],"readonly");
      const totals = [0,0,0,0]; let saved = [0,0,0,0], failure;
      const scan = (name, visit) => {
        const request = tx.objectStore(name).openCursor();
        request.onsuccess = () => {
          try { const cursor = request.result; if (!cursor) return; visit(cursor.key,cursor.value); cursor.continue(); }
          catch (error) { failure = error; tx.abort(); }
        };
      };
      scan("drafts", (key,value) => { const record = validateRecord(value); require(equal(key,record.key)); usage(record).forEach((cost,index) => totals[index] += cost); validateCounters(totals); });
      scan("usage", (key,value) => { require(key === "aggregate"); saved = validateCounters(value); });
      tx.oncomplete = () => equal(totals,saved) ? resolve() : reject(new Error("Saved draft counters are inconsistent; records were left unchanged"));
      tx.onabort = () => reject(failure ?? tx.error);
    });
  }

  // No await inside a transaction: read, CAS, record and counters share its commit.
  async mutate(key, change) {
    const [result] = await this.mutateMany([key], records => [change(records[0])]);
    return result;
  }
  async mutateMany(keys, change) {
    require(keys.length > 0 && keys.length <= 2 && new Set(keys.map(key => JSON.stringify(key))).size === keys.length);
    require(keys.every(key => equal(key.slice(0, 2), this.namespace) && key[2] === keys[0][2]));
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction(["drafts", "usage"], "readwrite", { durability: "strict" });
      const records = tx.objectStore("drafts"), counters = tx.objectStore("usage");
      const saved = Array(keys.length);
      let counts, reads = 0, result, failure;
      const apply = () => {
        if (++reads !== keys.length + 1) return;
        try {
          saved.forEach(record => { if (record) validateRecord(record); });
          counts = validateCounters(counts ?? [0, 0, 0, 0]);
          const next = change(saved.map(record => record && clone(record)));
          require(next.length === keys.length);
          next.forEach((record, index) => { if (record) { validateRecord(record); require(equal(record.key, keys[index])); } });
          const total = counts.map((count, dimension) => saved.reduce((sum, record, index) => sum - usage(record)[dimension] + usage(next[index])[dimension], count));
          require(total.every((count, index) => integer(count, limits[index])), "Saved drafts exceed the origin storage limit; clear an unused draft");
          next.forEach((record, index) => { if (record) records.put(record); else records.delete(keys[index]); });
          counters.put(total, "aggregate");
          result = next;
        } catch (error) { failure = error; tx.abort(); }
      };
      keys.forEach((key, index) => { const request = records.get(key); request.onsuccess = () => { saved[index] = request.result; apply(); }; });
      const count = counters.get("aggregate");
      count.onsuccess = () => { counts = count.result; apply(); };
      tx.oncomplete = () => resolve(result);
      tx.onabort = () => reject(failure ?? tx.error ?? new Error("Draft transaction was aborted; input was not saved"));
      tx.onerror = () => { /* onabort owns rejection and preserves the specific CAS error. */ };
    });
  }
  async get(pane, session) {
    const key = this.key(pane, session);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction("drafts", "readonly");
      const request = tx.objectStore("drafts").get(key);
      let result, failure;
      request.onsuccess = () => { try { result = request.result && validateRecord(request.result); } catch (error) { failure = error; } };
      tx.oncomplete = () => failure ? reject(failure) : resolve(result);
      tx.onabort = () => reject(tx.error);
    });
  }
  async list(pane) {
    const scope = this.key(pane, "list").slice(0, 3);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction("drafts", "readonly");
      const request = tx.objectStore("drafts").index("scope").openCursor(IDBKeyRange.only(scope));
      const result = [], total = [0, 0, 0, 0];
      let failure;
      request.onsuccess = () => {
        try {
          const cursor = request.result;
          if (!cursor) return;
          const record = validateRecord(cursor.value), cost = usage(record);
          cost.forEach((value, index) => { total[index] += value; });
          validateCounters(total);
          result.push(record); cursor.continue();
        } catch (error) { failure = error; }
      };
      tx.oncomplete = () => failure ? reject(failure) : resolve(result);
      tx.onabort = () => reject(tx.error);
    });
  }
  ensure(pane, session, header, creation) {
    const key = this.key(pane, session);
    return this.mutate(key, saved => saved ?? this.blank(pane, session, header, creation));
  }
  blank(pane, session, header, creation) {
    const key = this.key(pane, session);
    return validateRecord({
      version: 2, key, scope: key.slice(0, 3), incarnation: [...crypto.getRandomValues(new Uint8Array(16))].map(value => value.toString(16).padStart(2, "0")).join(""),
      editRevision: 0, pendingRevision: 0, text: "", images: [], header,
      creation: creation ?? null, everDispatched: !creation, pending: null, receipt: null,
    });
  }
  edit(expected, text, images) {
    validateEditor(text, images);
    return this.mutate(expected.key, saved => {
      sameIncarnation(expected, saved);
      if (saved.editRevision !== expected.editRevision) throw conflict(saved);
      saved.text = text; saved.images = clone(images); saved.editRevision = bump(saved.editRevision);
      return saved;
    });
  }
  freeze(expected, prepared) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      if (saved.pending) throw conflict(saved);
      saved.pending = { kind: prepared.kind, id: prepared.id, opaque: prepared.opaque,
        text_bytes: prepared.text_bytes, images: prepared.images, phase: "prepared", editRevision: expected.editRevision };
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  begin(expected) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending?.phase === "prepared", "This submission may already have executed; query its original result");
      saved.pending.phase = "dispatching"; saved.everDispatched = true;
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  settle(expected, settlement) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending && ["complete", "not_admitted", "unknown"].includes(settlement.status));
      if (settlement.status === "complete") {
        require(typeof settlement.receipt === "string", "The completed receipt must remain an opaque Rust string");
        saved.receipt = settlement.receipt;
        if (saved.editRevision === saved.pending.editRevision) {
          saved.text = ""; saved.images = []; saved.editRevision = bump(saved.editRevision);
        }
        saved.pending = null;
      } else { saved.pending.phase = settlement.status === "not_admitted" ? "prepared" : "unknown"; }
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  cancelPrepared(expected) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending?.phase === "prepared", "An uncertain submission cannot be discarded as unexecuted");
      saved.pending = null; saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  async moveFresh(expected, replacement) {
    const [, moved] = await this.mutateMany([expected.key, replacement.key], ([old, next]) => {
      samePending(expected, old); samePending(replacement, next);
      if (old.editRevision !== expected.editRevision || next.editRevision !== replacement.editRevision) throw conflict(old);
      require(old.creation && !old.everDispatched && !old.pending, "Only a never-dispatched Fresh draft can start a replacement conversation");
      require(!next.text && !next.images.length && !next.pending && next.creation && !next.everDispatched);
      require(old.creation.workspace_id === next.creation.workspace_id && old.creation.workspace_trust === next.creation.workspace_trust);
      next.text = old.text; next.images = old.images; next.editRevision = bump(next.editRevision);
      return [undefined, next];
    });
    return moved;
  }
  removeEmpty(expected) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      if (saved.editRevision !== expected.editRevision) throw conflict(saved);
      require(!saved.text && !saved.images.length && !saved.pending, "Clear this draft before removing its record");
      return undefined;
    });
  }
}

// One bounded local editor coalesces persistence without retaining an event queue.
// Pending operations serialize with saves; edits continue while a request runs.
export class DraftEditor {
  constructor(store, record, changed = () => {}) {
    this.store = store; this.record = record; this.text = record.text; this.images = clone(record.images);
    this.changed = changed; this.revision = 0; this.dirty = false; this.work = Promise.resolve();
  }
  get text() { return this.localText; }
  set text(value) { this.localText = value; this.textBytes = bytes(value); }
  edit(text, images = this.images) {
    require(!this.transferring, "This draft is moving to another conversation; wait for recovery to finish");
    validateEditor(text, images);
    this.text = text; this.images = clone(images); this.revision = bump(this.revision); this.dirty = true;
    this.pump(); this.changed();
  }
  enqueue(operation) {
    const result = this.work.then(operation);
    this.work = result.catch(() => {});
    return result;
  }
  pump() {
    if (this.saving || !this.dirty || this.failure) return;
    this.saving = this.enqueue(async () => {
      while (this.dirty && !this.failure) {
        const revision = this.revision;
        try {
          this.record = await this.store.edit(this.record, this.text, this.images);
          if (this.revision === revision) this.dirty = false;
        } catch (error) { this.failure = error; }
      }
    }).finally(() => { this.saving = undefined; this.changed(); });
  }
  async flush() {
    this.pump(); await this.work;
    if (this.failure) throw this.failure;
    if (this.dirty) throw new Error("Draft is not saved; sending is blocked");
  }
  update(operation) {
    if (this.transferring) return Promise.reject(new Error("This draft is moving to another conversation"));
    return this.enqueue(async () => {
      const revision = this.revision;
      try {
        this.record = await operation(this.record);
        if (!this.dirty && revision === this.revision) {
          this.text = this.record.text; this.images = clone(this.record.images);
        }
        this.changed(); return this.record;
      } catch (error) { this.failure = error; this.changed(); throw error; }
    });
  }
  async resolve(useSaved) {
    require(!this.transferring, "This draft is moving to another conversation");
    require(!this.resolving, "Wait for this draft's current conflict resolution");
    this.resolving = true;
    try {
      await this.work;
      let saved = await this.store.get(this.record.key[2], this.record.key[3]);
      if (this.record.pending && (!saved || saved.incarnation !== this.record.incarnation)) throw conflict(saved);
      saved ??= await this.store.ensure(this.record.key[2], this.record.key[3], this.record.header,
        this.record.everDispatched ? null : this.record.creation);
      this.record = saved;
      if (useSaved) { this.text = saved.text; this.images = clone(saved.images); this.dirty = false; }
      else { this.dirty = true; }
      this.failure = undefined; this.pump(); this.changed();
      await this.flush();
    } finally { this.resolving = false; }
  }
  async transfer(operation) {
    require(!this.transferring, "This draft is already moving to another conversation");
    require(!this.resolving, "Wait for this draft's current conflict resolution");
    this.transferring = true; this.changed();
    try { await this.flush(); return await operation(this.record); }
    finally { this.transferring = false; this.changed(); }
  }
}
