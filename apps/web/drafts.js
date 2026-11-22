import {assertDraftStorageActive, openDraftDatabase, discardDraftDatabases, draftDatabaseNames} from './draft-databases.js';
import {storedNamespace,readNamespace} from './storage-namespace.js';
const MiB = 1024 * 1024;
const encoder = new TextEncoder();
const nonAscii = /[^\x00-\x7f]/u;
const bytes = value => nonAscii.test(value) ? encoder.encode(value).length : value.length;
const hex = (value, length = 64) => typeof value === "string" && value.length === length && /^[0-9a-f]+$/.test(value);
const integer = (value, maximum = Number.MAX_SAFE_INTEGER) => Number.isSafeInteger(value) && value >= 0 && value <= maximum;
const identity = value => typeof value === "string" && bytes(value) > 0 && bytes(value) <= 256 && !/[\s\u0000-\u001f\u007f]/u.test(value);
const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const bump = value => { if (!integer(value) || value === Number.MAX_SAFE_INTEGER) throw new Error("Draft revision exhausted"); return value + 1; };
const clone = value => structuredClone(value);
const empty = record => !record.text && !record.images.length && !record.references.length && !record.pending && !record.releases.length;
function require(condition, message = "Saved draft is invalid; it was left unchanged") { if (!condition) throw new Error(message); }

export function validateEditor(text, images, references = []) {
  require(typeof text === "string", "Draft text must be a string");
  const textBytes = bytes(text);
  require(textBytes <= MiB, "Draft exceeds 1 MiB of UTF-8 text");
  require(Array.isArray(images) && images.length <= 8, "A draft can hold at most eight images");
  for (const image of images) {
    require(image && Object.keys(image).sort().join() === "bytes,height,id,mime,width" && hex(image.id) && image.mime === "image/png" &&
      integer(image.bytes, 32 * MiB) && image.bytes > 0 && integer(image.width, 65535) && image.width > 0 &&
      integer(image.height, 65535) && image.height > 0 && image.width * image.height <= 100_000_000, "Saved image reference is invalid");
  }
  require(Array.isArray(references) && references.length <= 4, "A draft can hold at most four conversation references");
  let previewBytes = 0;
  for (const reference of references) {
    require(reference && Object.keys(reference).sort().join() === "metadata,preview,snapshot" && reference.snapshot && Object.keys(reference.snapshot).sort().join() === "byte_len,capture_id,sha256" && identity(reference.snapshot.capture_id) && bytes(reference.snapshot.capture_id) <= 64 && hex(reference.snapshot.sha256) && integer(reference.snapshot.byte_len, 8 * MiB) && reference.snapshot.byte_len > 0, "Invalid frozen conversation reference");
    require(typeof reference.preview === "string" && bytes(reference.preview) > 0 && bytes(reference.preview) <= 8192 && !/[\u0000\u007f]/u.test(reference.preview), "Invalid reference preview");
    const metadata = reference.metadata;
    const decimal = value => typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n;
    const keys = (value, expected) => value && Object.keys(value).sort().join() === expected;
    const binding = value => keys(value,"header_sha256,session_id") && identity(value.session_id) && hex(value.header_sha256);
    require(keys(metadata,"capture,source,target,text_bytes") && binding(metadata.target), "Invalid reference origin");
    const origin = metadata.source;
    require(origin?.kind === "native" ? keys(origin,"binding,kind") && binding(origin.binding) : origin?.kind === "observed" && keys(origin,"epoch,id,kind,owner") && identity(origin.owner) && identity(origin.id) && decimal(origin.epoch) && BigInt(origin.epoch) > 0n, "Invalid reference source");
    require(integer(metadata.text_bytes,MiB) && metadata.text_bytes >= bytes(reference.preview), "Invalid reference limits");
    const capture = metadata.capture;
    if (capture?.kind === "suffix") {
      const interval = capture.interval;
      require(origin.kind === "native" && keys(capture,"interval,kind") && keys(interval,"fact_prefix_sha256,omissions,retained_after_seq,retained_through_seq,scanned_after_seq,scanned_bytes,through_seq") && hex(interval.fact_prefix_sha256), "Invalid suffix capture");
      require([interval.through_seq,interval.scanned_after_seq,interval.retained_after_seq,interval.retained_through_seq].every(decimal), "Invalid reference interval");
      const through = BigInt(interval.through_seq), scanned = BigInt(interval.scanned_after_seq), first = BigInt(interval.retained_after_seq), last = BigInt(interval.retained_through_seq);
      require(scanned < through && through - scanned <= 1024n && first >= scanned && first < last && last <= through, "Invalid reference interval");
      require(integer(interval.scanned_bytes,16*MiB) && interval.scanned_bytes > 0, "Invalid reference limits");
      require(Array.isArray(interval.omissions) && interval.omissions.length <= 3 && new Set(interval.omissions).size === interval.omissions.length && interval.omissions.every(reason => ["fact_limit","scan_bytes","content_bytes"].includes(reason)), "Invalid reference omission state");
    } else {
      const selected = capture?.selection, record = selected?.record;
      require(capture?.kind === "selected" && keys(capture,"kind,selection") && keys(selected,"end,record,scanned_bytes,start,text_sha256,through_seq") && keys(record,"content_index,kind,sequence"), "Invalid selected capture");
      require(decimal(selected.through_seq) && decimal(record.sequence) && BigInt(record.sequence)>0n && BigInt(record.sequence)<=BigInt(selected.through_seq) && ["human","assistant","tool_evidence"].includes(record.kind) && integer(record.content_index,1023), "Invalid selected record");
      require(integer(selected.start,MiB) && integer(selected.end,MiB) && selected.start < selected.end && selected.end-selected.start===metadata.text_bytes && hex(selected.text_sha256) && integer(selected.scanned_bytes,16*MiB) && selected.scanned_bytes>0, "Invalid selected range");
    }
    require(bytes(JSON.stringify(reference)) <= 64 * 1024, "Reference descriptor exceeds its limit");
    previewBytes += bytes(reference.preview);
  }
  require(textBytes + previewBytes <= MiB, "Draft text and reference previews exceed 1 MiB");
  return textBytes + referenceBytes(references);
}
function validatePending(pending) {
  require(pending && Object.keys(pending).filter(key => key !== 'preserve_editor' && key !== 'consume').sort().join() === "captures,editRevision,id,images,kind,opaque,phase,text_bytes");
  require(!("preserve_editor" in pending) || pending.preserve_editor === true && pending.kind === "command");
  if ('consume' in pending) {
    require(pending.preserve_editor && pending.kind === 'command' && pending.consume && Object.keys(pending.consume).sort().join() === 'end,start');
    require(integer(pending.consume.start,MiB) && integer(pending.consume.end,MiB) && pending.consume.start < pending.consume.end);
  }
  require(pending && ["message", "command", "queue"].includes(pending.kind) && identity(pending.id));
  require(typeof pending.opaque === "string");
  const opaqueBytes = bytes(pending.opaque);
  require(opaqueBytes <= (pending.kind !== "command" ? 8 * MiB : 32 * 1024));
  require(integer(pending.text_bytes, MiB) && integer(pending.images, 8));
  validateCaptures(pending.captures, 4);
  require(["prepared", "dispatching", "unknown"].includes(pending.phase) && integer(pending.editRevision));
  return opaqueBytes;
}
export const captureOf = reference => ({target: reference.metadata.target, snapshot: reference.snapshot});
export const captureKey = capture => JSON.stringify([capture.target.session_id, capture.target.header_sha256, capture.snapshot.capture_id, capture.snapshot.sha256, capture.snapshot.byte_len]);
const sameCapture = (left, right) => captureKey(left) === captureKey(right);
function validateCaptures(captures, maximum) {
  require(Array.isArray(captures) && captures.length <= maximum);
  for (const capture of captures) {
    require(capture && Object.keys(capture).sort().join() === "snapshot,target" &&
      capture.target && Object.keys(capture.target).sort().join() === "header_sha256,session_id" &&
      identity(capture.target.session_id) && hex(capture.target.header_sha256) &&
      capture.snapshot && Object.keys(capture.snapshot).sort().join() === "byte_len,capture_id,sha256" &&
      identity(capture.snapshot.capture_id) && bytes(capture.snapshot.capture_id) <= 64 &&
      hex(capture.snapshot.sha256) && integer(capture.snapshot.byte_len, 8 * MiB) && capture.snapshot.byte_len > 0);
  }
  require(new Set(captures.map(captureKey)).size === captures.length);
}
const owners = record => [...record.references.map(captureOf), ...record.pending?.captures ?? []];
function captureCount(record, references) {
  return new Set([...owners(record), ...record.releases, ...references.map(captureOf)].map(captureKey)).size;
}
export function canCaptureReference(record, references) {
  return references.length < 4 && captureCount(record, references) < 16;
}
function retireCaptures(previousOwners, after) {
  const retained = new Set(owners(after).map(captureKey));
  const retired = new Map(after.releases.map(capture => [captureKey(capture), capture]));
  for (const capture of previousOwners) if (!retained.has(captureKey(capture))) retired.set(captureKey(capture), capture);
  after.releases = [...retired.values()];
}
const surface = value => typeof value === "string" && /^[a-z0-9_-]{1,32}$/.test(value);
const principalKey = value => value === "local" || (typeof value === "string" && /^device:[0-9a-f]{32}$/.test(value));
function validateCreation(creation, session) {
  require(creation && hex(creation.workspace_id) && creation.session_id === session);
  const fields = Object.keys(creation).sort().join();
  const explicit = typeof creation.agent_preset_id === "string" && /^[a-z0-9][a-z0-9-]{0,254}$/.test(creation.agent_preset_id);
  if (fields === "agent_preset_id,session_id,workspace_id") {
    require(creation.agent_preset_id === null || explicit);
    return;
  }
  require(fields === "agent_preset_id,fork,session_id,workspace_id" && explicit);
  const fork = creation.fork;
  require(fork && Object.keys(fork).sort().join() === "point,source_session_id,turns"
    && identity(fork.source_session_id) && fork.source_session_id !== session);
  const point = fork.point;
  require(point && Object.keys(point).sort().join() === "kind,turn_id"
    && ["before_turn", "through_turn"].includes(point.kind) && identity(point.turn_id));
  const selection = fork.turns;
  require(selection === "all" || selection === "none" || (selection && typeof selection === "object"
    && Object.keys(selection).join() === "last" && integer(selection.last) && selection.last > 0));
}
function validateRecord(record) {
  require(record?.version === 11 && Array.isArray(record.key) && record.key.length === 4 &&
    hex(record.key[0], 32) && (principalKey(record.key[1]) && surface(record.key[2])) && identity(record.key[3]));
  require(hex(record.header) && hex(record.incarnation, 32));
  require(integer(record.editRevision) && integer(record.pendingRevision) && typeof record.everDispatched === "boolean");
  const editorBytes = validateEditor(record.text, record.images, record.references);
  validateCaptures(record.releases, 16);
  require(record.references.every(reference => reference.metadata.target.session_id === record.key[3] && reference.metadata.target.header_sha256 === record.header), "Reference belongs to another draft Header");
  let opaqueBytes = 0;
  if (record.pending !== null) {
    opaqueBytes = validatePending(record.pending);
    require(record.pending.editRevision <= record.editRevision && (record.pending.phase === "prepared" || record.everDispatched));
  }
  const owned = new Set(owners(record).map(captureKey));
  require(new Set([...owned, ...record.releases.map(captureKey)]).size <= 16,
    "Reference cleanup is pending. Reconnect and retry cleanup before adding more references.");
  require([...record.releases, ...record.pending?.captures ?? []].every(capture => capture.target.session_id === record.key[3] && capture.target.header_sha256 === record.header));
  require(record.releases.every(capture => !owned.has(captureKey(capture))), "A retired capture cannot be restored; capture it again");
  if (record.creation !== null) validateCreation(record.creation, record.key[3]);
  require(Object.keys(record).sort().join() === "creation,editRevision,everDispatched,header,images,incarnation,key,pending,pendingRevision,references,releases,text,version");
  return [1, editorBytes, record.pending?.text_bytes ?? 0, opaqueBytes];
}
const referenceBytes = references => references?.length ? bytes(JSON.stringify(references)) : 0;
const emptyUsage = [0, 0, 0, 0];
const limits = [128, 4 * MiB, 4 * MiB, 32 * MiB];
function validateCounters(value) {
  require(Array.isArray(value) && value.length === limits.length && value.every((count, index) => integer(count, limits[index])));
  return value;
}
function conflict(saved) {
  return Object.assign(new Error("This draft changed in another editor. Choose the saved input or replace its text, images and references."), { code: "conflict", saved });
}
function sameIncarnation(expected, saved) {
  if (!saved || saved.incarnation !== expected.incarnation) throw conflict(saved);
}
function requireRetainedReferences(expected, saved, references) {
  if (!references.length) return;
  const previous = owners(expected), retained = owners(saved);
  for (const reference of references) {
    const capture = captureOf(reference);
    if (previous.some(owner => sameCapture(owner, capture)) &&
        !retained.some(owner => sameCapture(owner, capture))) {
      throw new Error("A reference was removed in another tab; use the saved input and capture it again");
    }
  }
}
function samePending(expected, saved) {
  sameIncarnation(expected, saved);
  if (saved.pendingRevision !== expected.pendingRevision) throw conflict(saved);
  if (saved.pending === expected.pending) return;
  if (!saved.pending || !expected.pending) throw conflict(saved);
  const {opaque: stored, ...storedFields} = saved.pending;
  const {opaque: wanted, ...wantedFields} = expected.pending;
  if (stored !== wanted || !equal(storedFields, wantedFields)) throw conflict(saved);
}

function editableFields({text, images, references, editRevision}) {
  return {text, images, references, editRevision};
}
function consumeSubmittedInput(input, pending) {
  if (input.editRevision !== pending.editRevision) return false;
  if (pending.consume) {
    const {start,end} = pending.consume;
    input.text = input.text.slice(0,start) + input.text.slice(end);
  } else if (!pending.preserve_editor && pending.kind !== "queue") {
    input.text = ""; input.images = []; input.references = [];
  } else return false;
  input.editRevision = bump(input.editRevision);
  return true;
}

// IndexedDB orders these four components lexicographically for pane-scoped cursors.
function cursorKey(key) {
  require(Array.isArray(key) && key.length === 4);
  const namespace = readNamespace(key[0],key[1]);
  require(namespace);
  return [...namespace,key[2],key[3]];
}
function withKey(value, key) {
  require(value && typeof value === 'object' && !Object.hasOwn(value,'key'));
  value.key = key;
  return value;
}
function putDraft(store, record, address) {
  const {key, ...content} = record;
  store.put(content, address);
}
function openDatabase() {
  return openDraftDatabase(draftDatabaseNames.composer, 11, (db, tx, oldVersion, fail) => {
    if (oldVersion === 0) {
      db.createObjectStore("drafts");
      db.createObjectStore("usage");
    } else if ([7,8,9,10].includes(oldVersion)) {
      const previous = tx.objectStore('drafts');
      const rekey = oldVersion < 10;
      if (rekey) previous.name = 'previousDrafts';
      const records = rekey ? db.createObjectStore('drafts') : previous;
      const request = previous.openCursor();
      request.onsuccess = () => {
        try {
          const cursor = request.result;
          if (!cursor) { if (rekey) db.deleteObjectStore(previous.name); return; }
          const record = rekey ? cursor.value : withKey(cursor.value,cursorKey(cursor.primaryKey));
          require(record.version === oldVersion);
          if (rekey) require(Array.isArray(record.key) && equal(cursor.primaryKey,record.key));
          if (oldVersion < 9) {
            require(equal(record.scope,record.key.slice(0,3)));
            delete record.scope;
          }
          delete record.receipt;
          record.version = 11;
          validateRecord(record);
          const [endpoint,principal,pane,session] = record.key;
          putDraft(records,record,rekey ? [...storedNamespace(endpoint,principal),pane,session] : cursor.primaryKey);
          cursor.continue();
        } catch (error) { fail(error); }
      };
    } else throw new Error("Unsupported saved draft schema");
  });
}

export class DraftStore {
  ephemeral = new WeakSet();
  static discardAllDrafts() { return discardDraftDatabases(); }
  static async open(endpoint, principal) {
    require(hex(endpoint,32) && principal && (principal.kind === "local" ? Object.keys(principal).length === 1 : principal.kind === "device" && Object.keys(principal).length === 2 && hex(principal.device_id,32)), "Authenticated draft namespace is unavailable");
    const key = principal.kind === "local" ? "local" : `device:${principal.device_id}`;
    const store = new DraftStore(await openDatabase(), endpoint, key);
    await store.verify(); return store;
  }
  constructor(db, endpoint, principal) {
    this.database = db; this.namespace = [endpoint, principal];
    this.storedNamespace = storedNamespace(endpoint,principal);
  }
  get db() { assertDraftStorageActive(); require(this.database, "Draft storage is unavailable. Input cannot be saved or sent."); return this.database; }
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
      scan("drafts", (key,value) => { const cost = validateRecord(withKey(value,cursorKey(key))); cost.forEach((amount,index) => totals[index] += amount); validateCounters(totals); });
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
    const addresses = keys.map(key => [...this.storedNamespace,key[2],key[3]]);
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction(["drafts", "usage"], "readwrite", { durability: "strict" });
      const records = tx.objectStore("drafts"), counters = tx.objectStore("usage");
      const saved = Array(keys.length);
      let counts, reads = 0, result, failure;
      const apply = () => {
        if (++reads !== keys.length + 1) return;
        try {
          const before = saved.map((record,index) => record === undefined ? emptyUsage : validateRecord(withKey(record,keys[index])));
          const previousOwners = saved.map(record => record && owners(record));
          counts = validateCounters(counts ?? [0, 0, 0, 0]);
          const next = change(saved);
          require(next.length === keys.length);
          const after = next.map((record, index) => {
            if (!record) return emptyUsage;
            if (previousOwners[index]?.length) retireCaptures(previousOwners[index], record);
            require(equal(record.key, keys[index]));
            return validateRecord(record);
          });
          const total = counts.map((count, dimension) => before.reduce((sum, cost, index) => sum - cost[dimension] + after[index][dimension], count));
          const finish = () => {
            require(total.every((count, index) => integer(count, limits[index])), "Saved drafts exceed the origin storage limit; open Restore drafts and delete an unused draft");
            next.forEach((record, index) => { if (record) putDraft(records,record,addresses[index]); else records.delete(addresses[index]); });
            if (total.some((count, index) => count !== counts[index])) counters.put(total, "aggregate");
            result = next;
          };
          if (total[0] <= limits[0] || total.slice(1).some((count, index) => !integer(count, limits[index + 1]))) return finish();
          const scan = records.openCursor();
          scan.onsuccess = () => {
            try {
              const cursor = scan.result;
              if (!cursor || total[0] <= limits[0]) return finish();
              const record = withKey(cursor.value,cursorKey(cursor.primaryKey));
              validateRecord(record);
              if (equal(record.key.slice(0, 2), this.namespace) && !keys.some(key => equal(key, record.key)) && record.everDispatched && empty(record)) {
                cursor.delete(); total[0]--;
              }
              cursor.continue();
            } catch (error) { failure = error; tx.abort(); }
          };
        } catch (error) { failure = error; tx.abort(); }
      };
      addresses.forEach((address, index) => { const request = records.get(address); request.onsuccess = () => { saved[index] = request.result; apply(); }; });
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
      const request = tx.objectStore("drafts").get([...this.storedNamespace,pane,session]);
      let result, failure;
      request.onsuccess = () => { try { if (request.result !== undefined) { result = withKey(request.result,key); validateRecord(result); } } catch (error) { failure = error; } };
      tx.oncomplete = () => failure ? reject(failure) : resolve(result);
      tx.onabort = () => reject(tx.error);
    });
  }
  async list(pane) {
    require(surface(pane));
    const scope = [...this.storedNamespace,pane];
    return new Promise((resolve, reject) => {
      const tx = this.db.transaction("drafts", "readonly");
      // Session IDs are strings. An array sorts after every possible string key.
      const request = tx.objectStore("drafts").openCursor(IDBKeyRange.bound(scope, [...scope, []], false, true));
      const result = [], total = [0, 0, 0, 0];
      let failure;
      request.onsuccess = () => {
        try {
          const cursor = request.result;
          if (!cursor) return;
          const record = withKey(cursor.value,cursorKey(cursor.primaryKey)), cost = validateRecord(record);
          cost.forEach((value, index) => { total[index] += value; });
          validateCounters(total);
          result.push(record); cursor.continue();
        } catch (error) { failure = error; }
      };
      tx.oncomplete = () => failure ? reject(failure) : resolve(result);
      tx.onabort = () => reject(tx.error);
    });
  }
  async ensure(pane, session, header, creation) {
    const saved = await this.get(pane, session);
    if (saved) return saved;
    return this.blank(pane, session, header, creation);
  }
  materialize(expected, saved) {
    return saved ?? (this.ephemeral.has(expected) ? clone(expected) : undefined);
  }
  async mutateInitial(expected, change) {
    const result = await this.mutate(expected.key, saved => change(this.materialize(expected, saved)));
    this.ephemeral.delete(expected);
    return result;
  }
  blank(pane, session, header, creation) {
    const key = this.key(pane, session);
    const blank = {
      version: 11, key, incarnation: [...crypto.getRandomValues(new Uint8Array(16))].map(value => value.toString(16).padStart(2, "0")).join(""),
      editRevision: 0, pendingRevision: 0, text: "", images: [], references: [], releases: [], header,
      creation: creation ?? null, everDispatched: !creation, pending: null,
    };
    validateRecord(blank);
    this.ephemeral.add(blank);
    return blank;
  }
  edit(expected, text, images, references = expected.references) {
    validateEditor(text, images, references);
    return this.mutateInitial(expected, saved => {
      if (!this.ephemeral.has(expected) || expected.header !== saved.header) sameIncarnation(expected, saved);
      if (saved.editRevision !== expected.editRevision) requireRetainedReferences(expected, saved, references);
      saved.text = text; saved.images = clone(images); saved.references = clone(references); saved.editRevision = bump(saved.editRevision);
      return saved;
    });
  }
  rebindPreset(expected, header, preset) {
    return this.mutateInitial(expected, saved => {
      samePending(expected, saved);
      require(saved.header === expected.header && !saved.pending && !saved.references.length && !saved.releases.length,
        "Resolve pending input and release references before changing the draft preset");
      saved.header = header;
      if (saved.creation) saved.creation.agent_preset_id = preset;
      saved.incarnation = crypto.randomUUID().replaceAll('-', '');
      return saved;
    });
  }
  startDispatch(expected, prepared, preserveEditor = false, consume) {
    require(!preserveEditor || prepared.kind === "command", "Only a command control can retain its editor");
    require(!consume || preserveEditor && integer(consume.start) && consume.start < consume.end && consume.end <= expected.text.length, 'Invalid command trigger');
    return this.mutateInitial(expected, saved => {
      samePending(expected, saved);
      if (saved.pending) throw conflict(saved);
      validateCaptures(prepared.captures, 4);
      if (prepared.kind !== "queue" && prepared.captures.some(capture => !saved.references.some(reference => sameCapture(captureOf(reference), capture)))) throw conflict(saved);
      saved.pending = { kind: prepared.kind, id: prepared.id, opaque: prepared.opaque,
        ...(preserveEditor ? {preserve_editor: true} : {}),
        ...(consume ? {consume:clone(consume)} : {}),
        text_bytes: prepared.text_bytes, images: prepared.images, captures: clone(prepared.captures), phase: "dispatching", editRevision: expected.editRevision };
      saved.everDispatched = true;
      saved.creation = null;
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  claimPrepared(expected) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending?.phase === "prepared", "This submission may already have executed; query its original result");
      saved.pending.phase = "dispatching"; saved.everDispatched = true; saved.creation = null;
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  async settle(expected, settlement) {
    return (await this.settleRequest(expected, settlement)).record;
  }
  async settleRequest(expected, settlement) {
    const observedPhase = settlement.status === "not_admitted" ? "prepared" : settlement.status === "unknown" ? "unknown" : undefined;
    if (observedPhase && expected.pending?.phase === observedPhase) {
      require(equal(expected.key.slice(0, 2), this.namespace));
      const saved = await this.get(expected.key[2], expected.key[3]);
      samePending(expected, saved);
      return {record:saved, consumed:false};
    }
    let consumed = false;
    const record = await this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending && ["complete", "rejected", "not_admitted", "unknown"].includes(settlement.status));
      if (settlement.status === "complete") {
        consumed = consumeSubmittedInput(saved, saved.pending);
        saved.pending = null;
      } else if (settlement.status === "rejected") {
        require(saved.pending.kind === "queue", "Only a queue conflict can retire a rejected intent");
        saved.pending = null;
      } else { saved.pending.phase = observedPhase; }
      saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
    return {record, consumed};
  }
  cancelPrepared(expected) {
    return this.mutate(expected.key, saved => {
      samePending(expected, saved);
      require(saved.pending?.phase === "prepared", "An uncertain submission cannot be discarded as unexecuted");
      saved.pending = null; saved.pendingRevision = bump(saved.pendingRevision);
      return saved;
    });
  }
  acknowledgeRelease(expected, capture) {
    return this.mutate(expected.key, saved => {
      sameIncarnation(expected, saved);
      saved.releases = saved.releases.filter(item => !sameCapture(item, capture));
      return saved;
    });
  }
  retireUnowned(expected, captures) {
    validateCaptures(captures, 4);
    // Saved owners retire atomically; only a capture removed before its first save needs this write.
    const retained = [...owners(expected), ...expected.releases];
    if (captures.every(capture => retained.some(item => sameCapture(item, capture)))) return Promise.resolve(expected);
    return this.mutateInitial(expected, saved => {
      sameIncarnation(expected, saved);
      const owned = owners(saved);
      for (const capture of captures) {
        if (!owned.some(item => sameCapture(item, capture)) && !saved.releases.some(item => sameCapture(item, capture))) saved.releases.push(clone(capture));
      }
      return saved;
    });
  }
  async moveFresh(expected, replacement) {
    const [, moved] = await this.mutateMany([expected.key, replacement.key], ([old, next]) => {
      next = this.materialize(replacement, next);
      samePending(expected, old); samePending(replacement, next);
      if (old.editRevision !== expected.editRevision || next.editRevision !== replacement.editRevision) throw conflict(old);
      require(old.creation && !old.everDispatched && !old.pending, "Only a never-dispatched Fresh draft can start a replacement conversation");
      require(!old.references.length && !old.releases.length, "References belong to the original conversation. Remove and release them before moving this draft, then capture them again.");
      require(!next.text && !next.images.length && !next.references.length && !next.releases.length && !next.pending && next.creation && !next.everDispatched);
      const from = old.creation, to = next.creation;
      require(from.workspace_id === to.workspace_id && from.agent_preset_id === to.agent_preset_id
        && from.fork?.source_session_id === to.fork?.source_session_id
        && equal(from.fork?.point,to.fork?.point) && equal(from.fork?.turns,to.fork?.turns),
        "Replacement draft must preserve its workspace, preset and selected history");
      next.text = old.text; next.images = old.images; next.editRevision = bump(next.editRevision);
      return [undefined, next];
    });
    this.ephemeral.delete(replacement);
    return moved;
  }
  removeEmpty(expected) {
    return this.mutate(expected.key, saved => {
      if (!saved) return undefined;
      samePending(expected, saved);
      if (saved.editRevision !== expected.editRevision) throw conflict(saved);
      require(empty(saved), "Clear and release this draft before removing its record");
      return undefined;
    });
  }
  removeSaved(expected) {
    return this.mutate(expected.key, saved => {
      if (!saved) return undefined;
      samePending(expected, saved);
      if (saved.editRevision !== expected.editRevision) throw conflict(saved);
      require(!saved.pending && !saved.references.length && !saved.releases.length,
        "Resolve pending requests and release references before removing this draft");
      return undefined;
    });
  }
}

// One bounded local editor coalesces persistence without retaining an event queue.
// Pending operations serialize with saves; edits continue while a request runs.
export class DraftEditor {
  constructor(store, record, changed = () => {}) {
    this.store = store; this.record = record; this.references = clone(record.references); this.text = record.text; this.images = clone(record.images);
    this.changed = changed; this.revision = 0; this.dirty = false; this.work = Promise.resolve();
  }
  get text() { return this.localText; }
  // A metadata-only transaction may observe a peer's later editor snapshot.
  // Keep only our editable fields when the two diverge, including the revision used to freeze a send.
  get savedInput() { return this.windowInput ? {...this.record, ...this.windowInput} : this.record; }
  set text(value) { this.localText = value; this.localTextBytes = bytes(value); }
  get references() { return this.localReferences; }
  set references(value) { this.localReferences = value; this.localReferenceBytes = referenceBytes(value); this.localPreviewBytes = value.reduce((total, reference) => total + bytes(reference.preview), 0); }
  get textBytes() { return this.localTextBytes + this.localReferenceBytes; }
  edit(text, images = this.images, references = this.references, checkBudget = () => {}) {
    require(!this.transferring, "This draft is moving to another conversation; wait for recovery to finish");
    require(!this.resolving, "Wait for this draft's current conflict resolution");
    let size;
    if (images === this.images && references === this.references) {
      require(typeof text === "string", "Draft text must be a string");
      const textBytes = bytes(text);
      require(textBytes + this.localPreviewBytes <= MiB, "Draft text and reference previews exceed 1 MiB");
      size = textBytes + this.localReferenceBytes;
    } else {
      size = validateEditor(text, images, references);
      require(captureCount(this.record, references) <= 16, "Clean up references before capturing more");
    }
    checkBudget(size);
    if (references !== this.references) this.references = clone(references);
    if (images !== this.images) this.images = clone(images);
    this.localText = text; this.localTextBytes = size - this.localReferenceBytes;
    this.revision = bump(this.revision); this.dirty = true;
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
          this.record = await this.store.edit(this.savedInput, this.text, this.images, this.references);
          this.windowInput = undefined;
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
    return this.transition(async record => ({record:await operation(record)}));
  }
  settle(expected, result) {
    return this.transition(() => this.store.settleRequest(expected, result),
      result.status === "complete" ? expected.pending : undefined);
  }
  transition(operation, completed) {
    if (this.transferring) return Promise.reject(new Error("This draft is moving to another conversation"));
    if (this.resolving) return Promise.reject(new Error("Wait for this draft's current conflict resolution"));
    return this.enqueue(async () => {
      const revision = this.revision;
      try {
        const before = this.savedInput;
        const {record, consumed} = await operation(this.record);
        if (!this.windowInput && record.editRevision !== before.editRevision) this.windowInput = editableFields(before);
        if (completed && before.editRevision === completed.editRevision) {
          const local = editableFields(before);
          if (consumeSubmittedInput(local, completed)) {
            // A newer restored draft can survive while this window clears its own submitted input.
            local.editRevision = before.editRevision;
            this.windowInput = consumed ? undefined : local;
            const shown = this.windowInput ?? record;
            if (!this.dirty && revision === this.revision) {
              if (this.text !== shown.text) this.revision = bump(this.revision);
              this.text = shown.text; this.images = clone(shown.images); this.references = clone(shown.references);
            }
          }
        }
        this.record = record;
        this.changed(); return this.record;
      } catch (error) { this.failure = error; this.changed(); throw error; }
    });
  }
  async resolve(useSaved) {
    require(!this.transferring, "This draft is moving to another conversation");
    require(!this.resolving, "Wait for this draft's current conflict resolution");
    this.resolving = true; this.changed();
    try {
      await this.work;
      let saved = await this.store.get(this.record.key[2], this.record.key[3]);
      if (this.record.pending && (!saved || saved.incarnation !== this.record.incarnation)) throw conflict(saved);
      saved ??= await this.store.ensure(this.record.key[2], this.record.key[3], this.record.header,
        this.record.everDispatched ? null : this.record.creation);
      if (!useSaved) {
        requireRetainedReferences(this.savedInput, saved, this.references);
      } else {
        saved = await this.store.retireUnowned(saved, this.references.map(captureOf));
      }
      this.record = saved;
      this.windowInput = undefined;
      if (useSaved) { if (this.text !== saved.text) this.revision = bump(this.revision); this.text = saved.text; this.images = clone(saved.images); this.references = clone(saved.references); this.dirty = false; }
      else { this.dirty = true; }
      this.failure = undefined; this.pump(); this.changed();
      await this.flush();
    } finally { this.resolving = false; this.changed(); }
  }
  async transfer(operation) {
    require(!this.transferring, "This draft is already moving to another conversation");
    require(!this.resolving, "Wait for this draft's current conflict resolution");
    this.transferring = true; this.changed();
    try { await this.flush(); return await operation(this.record); }
    finally { this.transferring = false; this.changed(); }
  }
}
