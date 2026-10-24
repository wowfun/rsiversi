import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export async function verifyDraftMigration(browser, root) {
  const source = await readFile(join(root, 'apps/web/drafts.js'), 'utf8');
  const reports = [];
  for (const version of [1, 2, 3, 4, 0]) {
    const context = await browser.newContext();
    await context.route('http://localhost:37918/**', route => route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('drafts.js') ? source : ''}));
    try {
      const page = await context.newPage(); await page.goto('http://localhost:37918/');
      const report = await page.evaluate(async version => {
        const sentinel = {opaque:'pending requests remain owned by their saved schema'};
        if (version) await new Promise((resolve,reject) => {
          const request = indexedDB.open('rsi.composer',version);
          request.onupgradeneeded = () => request.result.createObjectStore('sentinel').put(sentinel,'original');
          request.onsuccess = () => {request.result.close();resolve();}; request.onerror=()=>reject(request.error);
        });
        const {DraftStore} = await import('/drafts.js');
        let rejected = false, store;
        try {store = await DraftStore.open('a'.repeat(32),{kind:'local'});} catch {rejected=true;}
        if (version) {
          const db = await new Promise((resolve,reject) => {const request=indexedDB.open('rsi.composer');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
          const saved = await new Promise((resolve,reject) => {const request=db.transaction('sentinel').objectStore('sentinel').get('original');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
          const actual=db.version;db.close();return {rejected,version:actual,unchanged:JSON.stringify(saved)===JSON.stringify(sentinel)};
        }
        const local = await store.ensure('main','session','b'.repeat(64));
        const device = await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'c'.repeat(32)});
        const isolated = !await device.get('main','session') && local.key[1]==='local';
        await store.verify();
        const actual=store.database.version;store.database.close();device.database.close();
        return {rejected,version:actual,isolated};
      }, version);
      assert.deepEqual(report, version ? {rejected:true,version,unchanged:true} : {rejected:false,version:5,isolated:true});
      reports.push(report);
    } finally {await context.close();}
  }
  return reports;
}
