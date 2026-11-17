import {DraftStore} from '../drafts.js';
// Test isolation clears records; product reset deliberately retires the document.
export async function clearDraftRecords() {
  const store=await DraftStore.open('a'.repeat(32),{kind:'local'});
  await new Promise((resolve,reject)=>{
    const tx=store.database.transaction(['drafts','usage'],'readwrite');
    tx.objectStore('drafts').clear();tx.objectStore('usage').clear();
    tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error);
  });
}
