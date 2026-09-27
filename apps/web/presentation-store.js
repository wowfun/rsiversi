// Device layout only. Submission durability belongs to drafts.js and Rust.
export const defaultLayout = Object.freeze({version:2,navigationWidth:280,resourcesWidth:260,navigation:'expanded',resourcesClosed:true,detail:'standard',workspaces:[]});
const encoder = new TextEncoder();
const bytes = value => encoder.encode(JSON.stringify(value)).length;
export function validateLayout(value) {
  const fallback = () => ({...defaultLayout,workspaces:[]});
  if (!value || typeof value !== 'object' || Array.isArray(value)) return fallback();
  if (Object.keys(value).sort().join() === 'navigationClosed,navigationWidth,resourcesClosed,resourcesWidth' &&
      typeof value.navigationClosed === 'boolean' && typeof value.resourcesClosed === 'boolean') {
    value = {...defaultLayout,navigation:value.navigationClosed?'rail':'expanded',navigationWidth:value.navigationWidth,resourcesWidth:value.resourcesWidth};
  }
  if (Object.keys(value).sort().join() !== Object.keys(defaultLayout).sort().join() || value.version!==2 ||
      !Number.isFinite(value.navigationWidth) || !Number.isFinite(value.resourcesWidth) ||
      !['expanded','rail','hidden'].includes(value.navigation) || typeof value.resourcesClosed!=='boolean' ||
      !['compact','standard','detailed','verbose'].includes(value.detail) || !Array.isArray(value.workspaces) || value.workspaces.length>16) return fallback();
  const ids=new Set();
  for(const item of value.workspaces) {
    if (!item || Object.keys(item).sort().join()!=='expanded,id' || typeof item.id!=='string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(item.id) || typeof item.expanded!=='boolean' || ids.has(item.id)) return fallback();
    ids.add(item.id);
  }
  return {...value,navigationWidth:Math.round(Math.max(264,Math.min(420,value.navigationWidth))),resourcesWidth:Math.round(Math.max(210,Math.min(420,value.resourcesWidth))),workspaces:value.workspaces.map(item=>({...item}))};
}
export function workspacePreference(layout,id,expanded) {
  return validateLayout({...layout,workspaces:[...layout.workspaces.filter(item=>item.id!==id),{id,expanded}].slice(-16)});
}
export function presentationKey(endpoint, principal) {
  if (typeof endpoint !== 'string' || !endpoint || endpoint.length > 1024) throw new Error('Invalid presentation endpoint');
  const owner = principal?.kind === 'local' ? 'local' : principal?.kind === 'device' && typeof principal.device_id === 'string' && principal.device_id.length <= 256 ? `device:${principal.device_id}` : undefined;
  if (!owner) throw new Error('Invalid presentation principal');
  return JSON.stringify([endpoint,owner]);
}
export function boundedRecords(records, key, layout, now) {
  if (records.length > 64 || !Number.isSafeInteger(now) || now < 0) throw new Error('Invalid presentation storage');
  const sizes = new Map(); let total = 0;
  for (const record of records) {
    if (!record || Object.keys(record).sort().join() !== 'key,layout,used' || typeof record.key !== 'string' || record.key.length > 1400 || sizes.has(record.key) || !Number.isSafeInteger(record.used) || record.used < 0) throw new Error('Invalid presentation record');
    const size = bytes(record);
    if (size > 4096) throw new Error('Invalid presentation record');
    sizes.set(record.key,size); total += size;
  }
  if (total > 256 * 1024) throw new Error('Presentation storage is full');
  const next = records.filter(record => record.key !== key).sort((a,b) => a.used - b.used || a.key.localeCompare(b.key));
  const replacement = {key,layout:validateLayout(layout),used:now};
  const size = bytes(replacement);
  if (size > 4096) throw new Error('Presentation record is too large');
  total += size - (sizes.get(key) ?? 0);
  next.push(replacement);
  let start = 0;
  while (next.length-start > 64 || total > 256*1024) total -= sizes.get(next[start++].key);
  return next.slice(start);
}
export class PresentationStore {
  constructor(database, key) { this.database = database; this.key = key; }
  static async open(key) {
    return new Promise((resolve,reject) => {
      const opening = indexedDB.open('rsi.presentation',1);
      opening.onupgradeneeded = () => opening.result.createObjectStore('layouts',{keyPath:'key'});
      opening.onerror = () => reject(opening.error);
      opening.onsuccess = () => { opening.result.onversionchange = () => opening.result.close(); resolve(new PresentationStore(opening.result,key)); };
      opening.onblocked = () => reject(new Error('Presentation storage is blocked'));
    });
  }
  async load() {
    return new Promise((resolve,reject) => {
      const tx = this.database.transaction('layouts','readwrite'), store = tx.objectStore('layouts');
      const read = store.getAll(undefined,65); let layout;
      read.onsuccess = () => {
        try {
          layout = validateLayout(read.result.find(record => record.key === this.key)?.layout);
          const next = boundedRecords(read.result,this.key,layout,Date.now());
          store.clear(); for (const record of next) store.put(record);
        } catch { tx.abort(); }
      };
      tx.oncomplete = () => resolve(layout);
      tx.onabort = tx.onerror = () => reject(tx.error ?? new Error('Invalid presentation storage'));
    });
  }
  async save(layout) {
    return new Promise((resolve,reject) => {
      const tx = this.database.transaction('layouts','readwrite'), store = tx.objectStore('layouts');
      const read = store.getAll(undefined,65);
      read.onsuccess = () => {
        try { const next = boundedRecords(read.result,this.key,layout,Date.now()); store.clear(); for (const record of next) store.put(record); }
        catch { tx.abort(); }
      };
      tx.oncomplete = () => resolve();
      tx.onabort = tx.onerror = () => reject(tx.error ?? new Error('Invalid presentation storage'));
    });
  }
  close() { this.database.close(); }
}
