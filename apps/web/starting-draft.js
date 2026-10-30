// A pre-Session draft has no Session Header or submission identity. Keep it in
// its own store and use atomic revisions when two documents share a surface.
let database;
function open() {
  return database ??= new Promise((resolve, reject) => {
    const request = indexedDB.open('rsi.composer-starts', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('drafts');
    request.onerror = () => { database = undefined; reject(request.error); };
    request.onblocked = () => { database = undefined; reject(new Error('Close other windows to open draft storage')); };
    request.onsuccess = () => {
      request.result.onversionchange = () => { request.result.close(); database = undefined; };
      resolve(request.result);
    };
  });
}
export function validateStart(value) {
  if (!value || !Number.isSafeInteger(value.revision) || value.revision < 0 ||
      typeof value.text !== 'string' || new TextEncoder().encode(value.text).length > 1024 * 1024 ||
      !(value.workspace === null || (typeof value.workspace === 'string' && value.workspace.length <= 256)))
    throw new Error('Invalid starting draft');
  return value;
}
async function transaction(key, update) {
  const db = await open();
  return new Promise((resolve, reject) => {
    const tx = db.transaction('drafts', update ? 'readwrite' : 'readonly');
    const store = tx.objectStore('drafts'), request = store.get(key);
    let result, failure;
    request.onsuccess = () => {
      try {
        result = validateStart(request.result ?? {revision:0, text:'', workspace:null});
        if (update) { result = validateStart(update(result)); store.put(result, key); }
      } catch (error) { failure = error; tx.abort(); }
    };
    tx.oncomplete = () => resolve(result);
    tx.onabort = tx.onerror = () => reject(failure ?? tx.error ?? new Error('Starting draft was not saved'));
  });
}
export class StartingDraft {
  static async load(namespace, surface) {
    if (!Array.isArray(namespace) || namespace.length !== 2 || namespace.some(value => typeof value !== 'string' || value.length > 512) || !/^[\w-]{1,64}$/.test(surface))
      throw new Error('Invalid starting draft namespace');
    const key = JSON.stringify([...namespace, surface]);
    return new StartingDraft(key, await transaction(key));
  }
  constructor(key, record) { this.key = key; this.record = record; this.text = record.text; this.workspace = record.workspace; this.serial = 0; }
  edit(text, workspace = this.workspace) {
    validateStart({revision:0, text, workspace});
    this.text = text; this.workspace = workspace; this.serial++; this.dirty = true;
    this.changed?.(); void this.save().catch(() => {});
  }
  save() {
    if (this.saving) return this.saving;
    this.saving = (async () => {
      while (this.dirty && !this.failure) {
        const serial = this.serial, text = this.text, workspace = this.workspace, revision = this.record.revision;
        this.record = await transaction(this.key, saved => {
          if (saved.revision !== revision) throw new Error('This draft changed in another window');
          return {revision:revision + 1, text, workspace};
        });
        if (serial === this.serial) this.dirty = false;
      }
    })().catch(error => { this.failure = error; throw error; }).finally(() => { this.saving = undefined; this.changed?.(); });
    return this.saving;
  }
  async flush() { await this.save(); if (this.failure) throw this.failure; }
  async resolve(useSaved) {
    this.record = await transaction(this.key);
    if (useSaved) { this.text = this.record.text; this.workspace = this.record.workspace; this.serial++; this.dirty = false; }
    this.failure = undefined; this.changed?.(); await this.flush();
  }
  async clearCaptured(serial) {
    if (serial !== this.serial) return;
    this.edit(''); await this.flush();
  }
}
