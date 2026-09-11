import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export async function verifyDraftMigration(browser, root) {
  const source = await readFile(join(root,'plugins/rsi/web/drafts.js'),'utf8');
  const reports=[];
  for (const corrupt of [false,true]) {
    const context=await browser.newContext();
    await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('drafts.js') ? source : ''}));
    const page=await context.newPage();
    try {
      await page.goto('http://localhost:37918/');
      const report=await page.evaluate(async corrupt=>{
        const records=[0,1].map(pane=>({version:1,key:['a'.repeat(32),'b'.repeat(32),pane,`session-${pane}`],scope:['a'.repeat(32),'b'.repeat(32),pane],header:'c'.repeat(64),incarnation:String(pane+1).repeat(32),editRevision:3,pendingRevision:2,text:`草稿 ${pane}`,images:[],creation:null,everDispatched:true,pending:{kind:pane===0?'message':'command',id:`original-${pane}`,opaque:'{"revision":18446744073709551615,"fields":{"z":1,"a":2}}',text_bytes:10,images:0,phase:'unknown',editRevision:1},receipt:'{"seq":18446744073709551615}'}));
        await new Promise((resolve,reject)=>{
          const request=indexedDB.open('rsi.composer',1);
          request.onupgradeneeded=()=>{const db=request.result;const rows=db.createObjectStore('drafts',{keyPath:'key'});rows.createIndex('scope','scope');const usage=db.createObjectStore('usage');records.forEach((record,pane)=>{rows.add(record);usage.put([1,new TextEncoder().encode(record.text).length,10,new TextEncoder().encode(record.pending.opaque).length],pane)});if(corrupt)usage.put([0,0,0,0],1)};
          request.onsuccess=()=>{request.result.close();resolve()};request.onerror=()=>reject(request.error);
        });
        const {DraftStore}=await import('/drafts.js');
        let rejected=false,store;
        try {store=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'b'.repeat(32)})}catch {rejected=true}
        if(corrupt) {
          const raw=await new Promise((resolve,reject)=>{const request=indexedDB.open('rsi.composer');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
          const rows=await new Promise((resolve,reject)=>{const request=raw.transaction('drafts','readonly').objectStore('drafts').getAll();request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
          const unchanged=JSON.stringify(rows)===JSON.stringify(records),version=raw.version;raw.close();return {rejected,unchanged,version};
        }
        const migrated=await Promise.all(['main','compare'].map((surface,index)=>store.get(surface,`session-${index}`)));
        const expected=records.map((record,index)=>({...record,version:2,key:['a'.repeat(32),'device:'+'b'.repeat(32),index===0?'main':'compare',record.key[3]],scope:['a'.repeat(32),'device:'+'b'.repeat(32),index===0?'main':'compare']}));
        const preserved=JSON.stringify(migrated)===JSON.stringify(expected);
        const local=await DraftStore.open('a'.repeat(32),{kind:'local'}),device=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
        const isolated=!await local.get('main','session-0') && !await device.get('main','session-0');
        await local.ensure('main','session-0','e'.repeat(64));
        const realPrincipal=(await local.get('main','session-0')).key[1]==='local' && (await store.get('main','session-0')).pending.id==='original-0';
        await store.verify();return {rejected,preserved,isolated,realPrincipal,version:store.db.version};
      },corrupt);
      if(corrupt) assert.deepEqual(report,{rejected:true,unchanged:true,version:1});
      else assert.deepEqual(report,{rejected:false,preserved:true,isolated:true,realPrincipal:true,version:2});
      reports.push(report);
    } finally {await context.close()}
  }
  return reports;
}
