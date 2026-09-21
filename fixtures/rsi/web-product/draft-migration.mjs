import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export async function verifyDraftMigration(browser, root) {
  const source = await readFile(join(root,'plugins/rsi/web/drafts.js'),'utf8');
  const reports=[];
  for (const version of [1,2]) for (const phase of ["prepared","dispatching","unknown"]) for (const corrupt of [false,true]) {
    const context=await browser.newContext();
    await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('drafts.js') ? source : ''}));
    const page=await context.newPage();
    try {
      await page.goto('http://localhost:37918/');
      const report=await page.evaluate(async ({corrupt,version,phase})=>{
        const records=[0,1].map(pane=>({version,key:['a'.repeat(32),'b'.repeat(32),pane,`session-${pane}`],scope:['a'.repeat(32),'b'.repeat(32),pane],header:'c'.repeat(64),incarnation:String(pane+1).repeat(32),editRevision:3,pendingRevision:2,text:`草稿 ${pane}`,images:[{id:"d".repeat(64),mime:"image/png",bytes:72,width:1,height:1}],creation:{session_id:`session-${pane}`,workspace_id:"e".repeat(64),agent_preset_id:null,workspace_trust:pane===0 ? "trusted" : "untrusted"},everDispatched:true,pending:{kind:pane===0?'message':'command',id:`original-${pane}`,opaque:'{"revision":18446744073709551615,"fields":{"z":1,"a":2}}',text_bytes:10,images:1,phase,editRevision:1},receipt:'{"seq":18446744073709551615}'}));
        if (version >= 2) for (const record of records) { record.key[1] = `device:${record.key[1]}`; record.key[2] = record.key[2] === 0 ? "main" : "compare"; record.scope = record.key.slice(0,3); }
        const editableBytes = record => new TextEncoder().encode(record.text).length + (record.references?.length ? new TextEncoder().encode(JSON.stringify(record.references)).length : 0);
        await new Promise((resolve,reject)=>{
          const request=indexedDB.open('rsi.composer',version);
          request.onupgradeneeded=()=>{const db=request.result;const rows=db.createObjectStore('drafts',{keyPath:'key'});rows.createIndex('scope','scope');const usage=db.createObjectStore('usage');records.forEach(record=>rows.add(record)); if(version===1) records.forEach((record,pane)=>usage.put([1,editableBytes(record),10,new TextEncoder().encode(record.pending.opaque).length],pane)); else usage.put([2,records.reduce((sum,record)=>sum+editableBytes(record),0),20,records.reduce((sum,record)=>sum+new TextEncoder().encode(record.pending.opaque).length,0)],'aggregate'); if(corrupt)usage.put([0,0,0,0],version===1 ? 1 : 'aggregate')};
          request.onsuccess=()=>{request.result.close();resolve()};request.onerror=()=>reject(request.error);
        });
        const {DraftStore}=await import('/drafts.js');
        let rejected=false,store;
        try {store=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'b'.repeat(32)})}catch {rejected=true}
        if(corrupt) {
          const raw=await new Promise((resolve,reject)=>{const request=indexedDB.open('rsi.composer');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
          const rows=await new Promise((resolve,reject)=>{const request=raw.transaction('drafts','readonly').objectStore('drafts').getAll();request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
          const unchanged=JSON.stringify(rows)===JSON.stringify([...records].sort((a,b)=>JSON.stringify(a.key).localeCompare(JSON.stringify(b.key)))),version=raw.version;raw.close();return {rejected,unchanged,version};
        }
        const migrated=await Promise.all(['main','compare'].map((surface,index)=>store.get(surface,`session-${index}`)));
        const expected=records.map((record,index)=>({...record,version:4,creation:{session_id:record.creation.session_id,workspace_id:record.creation.workspace_id,agent_preset_id:null},pending:{...record.pending,references:record.pending.references??0},references:record.references??[],key:['a'.repeat(32),'device:'+'b'.repeat(32),index===0?'main':'compare',record.key[3]],scope:['a'.repeat(32),'device:'+'b'.repeat(32),index===0?'main':'compare']}));
        const normalize = value => Array.isArray(value) ? value.map(normalize) : value && typeof value === "object" ? Object.fromEntries(Object.keys(value).sort().map(key=>[key,normalize(value[key])])) : value;
        const preserved=JSON.stringify(normalize(migrated))===JSON.stringify(normalize(expected));
        const local=await DraftStore.open('a'.repeat(32),{kind:'local'}),device=await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'d'.repeat(32)});
        const isolated=!await local.get('main','session-0') && !await device.get('main','session-0');
        await local.ensure('main','session-0','e'.repeat(64));
        const realPrincipal=(await local.get('main','session-0')).key[1]==='local' && (await store.get('main','session-0')).pending.id==='original-0';
        await store.verify();return {rejected,preserved,isolated,realPrincipal,version:store.db.version};
      },{corrupt,version,phase});
      if(corrupt) assert.deepEqual(report,{rejected:true,unchanged:true,version});
      else assert.deepEqual(report,{rejected:false,preserved:true,isolated:true,realPrincipal:true,version:4});
      reports.push(report);
    } finally {await context.close()}
  }
  // Unsupported schema rejection is separate from the validated migrations.
  const context = await browser.newContext();
  await context.route('http://localhost:37918/**', route => route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('drafts.js') ? source : ''}));
  try {
    const page = await context.newPage(); await page.goto('http://localhost:37918/');
    const report = await page.evaluate(async () => {
      const sentinel = {opaque: 'old schema bytes remain owned by their original format'};
      await new Promise((resolve,reject) => {
        const request=indexedDB.open('rsi.composer',3);
        request.onupgradeneeded=()=>request.result.createObjectStore('sentinel').put(sentinel,'original');
        request.onsuccess=()=>{request.result.close();resolve()};request.onerror=()=>reject(request.error);
      });
      const {DraftStore}=await import('/drafts.js');let rejected=false;
      try {await DraftStore.open('a'.repeat(32),{kind:'local'})} catch {rejected=true}
      const request=indexedDB.open('rsi.composer');
      const db=await new Promise((resolve,reject)=>{request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
      const read=db.transaction('sentinel').objectStore('sentinel').get('original');
      const saved=await new Promise((resolve,reject)=>{read.onsuccess=()=>resolve(read.result);read.onerror=()=>reject(read.error)});
      const version=db.version;db.close();return {rejected,version,unchanged:JSON.stringify(saved)===JSON.stringify(sentinel)};
    });
    assert.deepEqual(report,{rejected:true,version:3,unchanged:true});reports.push(report);
  } finally {await context.close()}
  return reports;
}
