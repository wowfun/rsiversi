import {assertDraftStorageActive, openDraftDatabase, draftDatabaseNames} from './draft-databases.js';

const MiB = 1024 * 1024, encoder = new TextEncoder();
const bytes = text => encoder.encode(text).length;
const limits = [128, 4 * MiB];
const empty = record => !record.text && record.workspace === null;
const blank = () => ({revision:0, incarnation:null, text:'', workspace:null});
const usage = record => record ? [1, bytes(record.text)] : [0, 0];
const bump = value => {
  if (!Number.isSafeInteger(value) || value < 0 || value === Number.MAX_SAFE_INTEGER) throw new Error('Starting draft revision exhausted');
  return value + 1;
};
function validateStart(value, fields) {
  if (!value || !Number.isSafeInteger(value.revision) || value.revision < 0 ||
      typeof value.text !== 'string' || bytes(value.text) > MiB ||
      !(value.workspace === null || (typeof value.workspace === 'string' && value.workspace.length <= 256)))
    throw new Error('Invalid starting draft');
  if (fields && Object.keys(value).sort().join() !== fields) throw new Error('Invalid starting draft fields');
  if (fields?.startsWith('incarnation,') && (typeof value.incarnation !== 'string' || value.incarnation.length !== 36))
    throw new Error('Invalid starting draft identity');
  return value;
}
function namespacePrefix(namespace) {
  if (!Array.isArray(namespace) || namespace.length !== 2 || namespace.some(value => typeof value !== 'string' || value.length > 512))
    throw new Error('Invalid starting draft namespace');
  return JSON.stringify(namespace).slice(0, -1) + ',';
}
function keyOf(namespace, surface) {
  namespacePrefix(namespace);
  if (typeof surface !== 'string' || !/^[\w-]{1,64}$/.test(surface)) throw new Error('Invalid starting draft surface');
  return JSON.stringify([...namespace, surface]);
}
function validateKey(key, namespace) {
  // Two 512-unit namespace fields, JSON escaping, one 64-byte surface and framing.
  if (typeof key !== 'string' || key.length > 2*512*6+64+10) throw new Error('Invalid starting draft key');
  let parts;
  try { parts=JSON.parse(key); } catch { throw new Error('Invalid starting draft key'); }
  if (!Array.isArray(parts) || parts.length !== 3 || key !== keyOf(parts.slice(0,2),parts[2]) ||
      (namespace && (parts[0] !== namespace[0] || parts[1] !== namespace[1])))
    throw new Error('Invalid starting draft key');
}
function counters(value) {
  if (!value || !Array.isArray(value.counts) || value.counts.length !== 2 ||
      !value.counts.every(n => Number.isSafeInteger(n) && n >= 0))
    throw new Error('Invalid starting draft counters');
  return value;
}
function open() {
  return openDraftDatabase(draftDatabaseNames.starting, 3, (db, tx, oldVersion, fail) => {
    if (oldVersion === 0) db.createObjectStore('drafts');
    if (oldVersion > 2) throw new Error('Unsupported starting draft schema');
    const meta = oldVersion === 2 ? tx.objectStore('usage') : db.createObjectStore('usage');
    if (oldVersion === 2) {
      const request = meta.get('aggregate');
      request.onsuccess = () => {
        try { meta.put({counts:counters(request.result).counts}, 'aggregate'); }
        catch (error) { fail(error); }
      };
    } else meta.put({counts:[0,0]}, 'aggregate');
    if (oldVersion === 1) {
      const counts = [0,0], cursor = tx.objectStore('drafts').openCursor();
      cursor.onsuccess = () => {
        try {
          const row = cursor.result;
          if (!row) { meta.put({counts}, 'aggregate'); return; }
          const record = validateStart(row.value,'revision,text,workspace');
          validateKey(row.key);
          if (empty(record)) row.delete();
          else {
            const weight = usage(record);
            counts.forEach((_,index) => counts[index] += weight[index]);
            counters({counts});
            row.update({...record,incarnation:crypto.randomUUID()});
          }
          row.continue();
        } catch (error) { fail(error); }
      };
    }
  });
}
async function transaction(key, expected, replacement) {
  const db = await open();
  assertDraftStorageActive();
  return new Promise((resolve, reject) => {
    const writing = expected !== undefined;
    const tx = db.transaction(['drafts','usage'], writing ? 'readwrite' : 'readonly');
    const records = tx.objectStore('drafts'), meta = tx.objectStore('usage');
    let record, total, ready = 0, result, failure;
    const apply = () => {
      if (++ready !== 2) return;
      try {
        total = counters(total);
        if (record) {
          validateStart(record,'incarnation,revision,text,workspace');
        }
        if (writing) {
          if (record ? record.incarnation !== expected.record.incarnation || record.revision !== expected.record.revision
              : expected.record.incarnation !== null)
            throw new Error('This draft changed in another window');
          const next = replacement === null ? undefined : validateStart({
            ...replacement, revision:bump(record?.revision ?? 0), incarnation:record?.incarnation ?? crypto.randomUUID(),
          });
          const retained = next && !empty(next) ? next : undefined;
          const before = usage(record), after = usage(retained);
          const counts = total.counts.map((count,index) => count - before[index] + after[index]);
          counters({counts});
          if (!counts.every((count,index) => count <= limits[index] || count <= total.counts[index]))
            throw new Error('Starting drafts exceed the origin storage limit; open Restore drafts and delete an unused draft');
          if (retained) records.put(retained,key);
          else if (record) records.delete(key);
          total.counts = counts; meta.put(total,'aggregate'); record = retained;
        }
        result = record ?? blank();
      } catch (error) { failure = error; tx.abort(); }
    };
    const read = records.get(key); read.onsuccess = () => { record = read.result; apply(); };
    const count = meta.get('aggregate'); count.onsuccess = () => { total = count.result; apply(); };
    tx.oncomplete = () => resolve(result);
    tx.onabort = () => reject(failure ?? tx.error ?? new Error('Starting draft was not saved'));
  });
}
export class StartingDraft {
  static async load(namespace, surface) {
    const key = keyOf(namespace,surface);
    return new StartingDraft(key, await transaction(key));
  }
  static async list(namespace, after) {
    const prefix = namespacePrefix(namespace);
    if (after !== undefined) validateKey(after, namespace);
    const db = await open();
    return new Promise((resolve,reject) => {
      const tx = db.transaction('drafts','readonly'), rows = [];
      const request = tx.objectStore('drafts').openCursor(IDBKeyRange.bound(after ?? prefix,prefix+'\uffff',!!after,true));
      let next, failure;
      request.onsuccess = () => {
        try {
          const cursor = request.result;
          if (!cursor) return;
          if (rows.length === 32) { next = rows.at(-1).key; return; }
          validateKey(cursor.key, namespace);
          rows.push({key:cursor.key,...validateStart(cursor.value,'incarnation,revision,text,workspace')}); cursor.continue();
        } catch(error) { failure=error; tx.abort(); }
      };
      tx.oncomplete = () => resolve({rows,next});
      tx.onabort = () => reject(failure ?? tx.error);
    });
  }
  static async remove(row) {
    validateKey(row.key);
    await transaction(row.key,{record:row},null);
  }
  constructor(key, record) {
    this.key = key; this.record = record;
    this.text = record.text; this.workspace = record.workspace; this.serial = 0;
  }
  edit(text, workspace = this.workspace) {
    assertDraftStorageActive();
    validateStart({revision:0, text, workspace});
    this.text = text; this.workspace = workspace; this.serial++; this.dirty = true;
    this.changed?.(); void this.save().catch(() => {});
  }
  save() {
    if (this.saving) return this.saving;
    if (this.resolving) return Promise.resolve();
    this.saving = (async () => {
      while (this.dirty && !this.failure) {
        const serial = this.serial, text = this.text, workspace = this.workspace;
        const next = await transaction(this.key, this, {text, workspace});
        this.record = next;
        if (serial === this.serial) this.dirty = false;
      }
    })().catch(error => { this.failure = error; throw error; }).finally(() => { this.saving = undefined; this.changed?.(); });
    return this.saving;
  }
  async flush() { await this.save(); if (this.failure) throw this.failure; }
  async resolve(useSaved) {
    if (this.resolving) throw new Error('Wait for the current draft resolution');
    const serial = this.serial;
    this.resolving = true;
    try {
      await this.saving?.catch(() => {});
      const next = await transaction(this.key);
      this.record = next;
      if (useSaved && serial === this.serial) { this.text = this.record.text; this.workspace = this.record.workspace; this.serial++; this.dirty = false; }
      this.failure = undefined;
    } finally { this.resolving = false; this.changed?.(); }
    await this.flush();
  }
  async clearCaptured(serial) {
    if (serial !== this.serial) return;
    this.edit('', null); await this.flush();
  }
}
