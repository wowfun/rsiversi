// Both draft formats share one document lifetime. Retired editors never reopen it.
export const draftDatabaseNames = {composer:'rsi.composer',starting:'rsi.composer-starts'};
const connections = new Map();
let retired = false;
export const draftStorageRetired = () => retired;
const channel = typeof window !== 'undefined' && typeof BroadcastChannel !== 'undefined'
  ? new BroadcastChannel('rsi:draft-storage') : undefined;
channel?.addEventListener('message', event => { if (event.data === 'retire') retire(); });

export function assertDraftStorageActive() {
  if (retired) throw new Error('Draft storage was reset or upgraded in another window. Reload before editing.');
}

function retire() {
  if (retired) return;
  retired = true;
  if (typeof window !== 'undefined') window.dispatchEvent(new Event('rsi:draft-storage-retired'));
  for (const connection of connections.values()) void connection.then(db => db.close(), () => {});
}

export function openDraftDatabase(name, version, upgrade) {
  assertDraftStorageActive();
  if (!connections.has(name)) {
    const opened = new Promise((resolve, reject) => {
      let failure;
      const request = indexedDB.open(name, version);
      request.onupgradeneeded = event => {
        const tx = request.transaction;
        const fail = error => {
          failure = new Error(`${error.message}. Saved drafts remain on this device. Discarding saved drafts permanently deletes them.`, {cause:error});
          tx.abort();
        };
        try { upgrade(request.result, tx, event.oldVersion, fail); }
        catch (error) { fail(error); }
      };
      request.onerror = () => reject(failure ?? request.error);
      request.onblocked = () => {
        failure = new Error('Draft storage is waiting for another window to close');
        reject(failure);
      };
      request.onsuccess = () => {
        const db = request.result;
        if (failure || retired) { db.close(); reject(failure ?? new Error('Draft storage was retired; reload before editing')); return; }
        db.onversionchange = () => { retire(); db.close(); };
        resolve(db);
      };
    }).catch(error => { connections.delete(name); throw error; });
    connections.set(name, opened);
  }
  return connections.get(name);
}

export async function discardDraftDatabases() {
  retire();
  channel?.postMessage('retire');
  await Promise.all([...connections.values()].map(opened => opened.then(db => db.close(), () => {})));
  const results = await Promise.allSettled(Object.values(draftDatabaseNames).map(name => new Promise((resolve, reject) => {
    const request = indexedDB.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(new Error(`${name}: ${request.error?.message ?? 'deletion failed'}`));
    request.onblocked = () => reject(new Error(`${name}: Close other RSIversi tabs, then retry discarding drafts`));
  })));
  const failed = results.filter(result => result.status === 'rejected');
  if (failed.length) throw new Error(failed.map(result => result.reason.message).join('; '));
}
