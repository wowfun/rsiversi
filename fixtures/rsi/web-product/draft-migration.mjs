import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

export async function verifyDraftMigration(browser, root) {
  const source = await readFile(join(root, 'apps/web/drafts.js'), 'utf8');
  const databases = await readFile(join(root, 'apps/web/draft-databases.js'), 'utf8');
  const reports = [];
  for (const version of [1, 2, 3, 4, 5, 6, 0]) {
    const context = await browser.newContext();
    await context.route('http://localhost:37918/**', route => route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('/drafts.js') ? source : route.request().url().endsWith('/draft-databases.js') ? databases : ''}));
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
        const local = await store.edit(await store.ensure('main','session','b'.repeat(64)), 'local retained input', []);
        const device = await DraftStore.open('a'.repeat(32),{kind:'device',device_id:'c'.repeat(32)});
        const isolated = !await device.get('main','session') && local.key[1]==='local';
        await device.edit(await device.ensure('main','session','b'.repeat(64)), 'device retained input', []);
        const keys = await new Promise((resolve,reject)=>{
          const rows=[], request=store.db.transaction('drafts').objectStore('drafts').openCursor();
          request.onerror=()=>reject(request.error);
          request.onsuccess=()=>{
            const cursor=request.result;
            if(!cursor){resolve(rows);return;}
            rows.push([cursor.key[0].byteLength,cursor.key[1].byteLength,cursor.key[2],cursor.key[3],Object.hasOwn(cursor.value,'key')]);
            cursor.continue();
          };
        });
        await store.verify();
        const actual=store.database.version;store.database.close();device.database.close();
        return {rejected,version:actual,isolated,keys};
      }, version);
      assert.deepEqual(report, version ? {rejected:true,version,unchanged:true} : {rejected:false,version:10,isolated:true,
        keys:[[16,0,'main','session',false],[16,16,'main','session',false]]});
      reports.push(report);
    } finally {await context.close();}
  }
  return reports;
}

export async function verifyDraftReset(browser, root) {
  const files=new Map(await Promise.all(['drafts.js','draft-databases.js','starting-draft.js'].map(async name=>[name,await readFile(join(root,'apps/web',name),'utf8')])));
  const context=await browser.newContext();
  await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:files.get(new URL(route.request().url()).pathname.slice(1))??''}));
  try {
    const first=await context.newPage(),second=await context.newPage(),blocker=await context.newPage();
    for(const page of [first,second,blocker])await page.goto('http://localhost:37918/');
    await first.evaluate(async()=>{
      const {DraftStore}=await import('/drafts.js'),{StartingDraft}=await import('/starting-draft.js');
      window.store=await DraftStore.open('a'.repeat(32),{kind:'local'});
      await store.edit(await store.ensure('main','session','b'.repeat(64)),'bound text',[]);
      const start=await StartingDraft.load(store.namespace,'main');start.edit('starting text');await start.flush();
      // Keep old endpoint drafts beside the endpoint minted by a fresh XDG home.
      const previous=await DraftStore.open('c'.repeat(32),{kind:'local'});
      await previous.edit(await previous.ensure('main','old-session','b'.repeat(64)),'old endpoint text',[]);
      const oldStart=await StartingDraft.load(previous.namespace,'main');oldStart.edit('old starting text');await oldStart.flush();
      if(await store.get('main','old-session'))throw new Error('Endpoint drafts leaked across namespaces');
    });
    await second.evaluate(async()=>{
      const {DraftStore}=await import('/drafts.js'),{StartingDraft}=await import('/starting-draft.js');
      window.store=await DraftStore.open('a'.repeat(32),{kind:'local'});window.start=await StartingDraft.load(store.namespace,'main');
    });
    await blocker.evaluate(()=>new Promise((resolve,reject)=>{
      const request=indexedDB.open('rsi.composer-starts');
      request.onsuccess=()=>{window.held=request.result;held.onversionchange=()=>{};resolve();};request.onerror=()=>reject(request.error);
    }));
    const blocked=await first.evaluate(async()=>{try{await (await import('/drafts.js')).DraftStore.discardAllDrafts();return '';}catch(error){return error.message;}});
    assert.match(blocked,/rsi.composer-starts: Close other RSIversi tabs/);
    await blocker.evaluate(()=>held.close());
    await first.evaluate(async()=>{await (await import('/drafts.js')).DraftStore.discardAllDrafts();});
    const retired=await second.evaluate(async()=>{
      let edit=false,read=false;
      try{start.edit('resurrection');}catch{edit=true;}
      try{await store.get('main','session');}catch{read=true;}
      return {edit,read,databases:(await indexedDB.databases()).map(db=>db.name)};
    });
    assert.deepEqual(retired,{edit:true,read:true,databases:[]});
    await first.reload();
    const fresh=await first.evaluate(async()=>{
      const {DraftStore}=await import('/drafts.js'),{StartingDraft}=await import('/starting-draft.js');
      const store=await DraftStore.open('a'.repeat(32),{kind:'local'});
      return {bound:!!await store.get('main','session'),starting:(await StartingDraft.load(store.namespace,'main')).text};
    });
    assert.deepEqual(fresh,{bound:false,starting:''});
    return {blockedReported:true,retiredAcrossTabs:true,bothDeleted:true,reloadEmpty:true};
  } finally {await context.close();}
}

export async function verifyDraftUpgradeErrors(browser, root) {
  const files=new Map(await Promise.all(['drafts.js','draft-databases.js','starting-draft.js'].map(async name=>[name,await readFile(join(root,'apps/web',name),'utf8')])));
  const reports=[];
  for(const starting of [false,true]) {
    const context=await browser.newContext();
    await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:files.get(new URL(route.request().url()).pathname.slice(1))??''}));
    try {
      const page=await context.newPage();await page.goto('http://localhost:37918/');
      const result=await page.evaluate(async starting=>{
        const {DraftStore}=await import('/drafts.js'),{StartingDraft}=await import('/starting-draft.js');
        const namespace=['a'.repeat(32),'local'],name=starting?'rsi.composer-starts':'rsi.composer',version=starting?1:7;
        const store=new DraftStore(undefined,...namespace);
        const first=starting?{revision:1,text:'retained',workspace:null}:{...store.blank('a','session','b'.repeat(64)),version:7};
        const invalid=starting?{revision:-1,text:'also retained',workspace:null}:{...store.blank('z','session','b'.repeat(64)),version:7,text:123};
        if(!starting){first.scope=first.key.slice(0,3);invalid.scope=invalid.key.slice(0,3);}
        await new Promise((resolve,reject)=>{
          const request=indexedDB.open(name,version);
          request.onupgradeneeded=()=>{
            const records=request.result.createObjectStore('drafts',starting?undefined:{keyPath:'key'});
            if(starting){records.put(first,JSON.stringify([...namespace,'a']));records.put(invalid,JSON.stringify([...namespace,'z']));}
            else {records.createIndex('scope','scope');records.put(first);records.put(invalid);request.result.createObjectStore('usage');}
          };
          request.onsuccess=()=>{request.result.close();resolve();};request.onerror=()=>reject(request.error);
        });
        let message;
        try{if(starting)await StartingDraft.load(namespace,'a');else await DraftStore.open(namespace[0],{kind:'local'});}catch(error){message=error.message;}
        const db=await new Promise((resolve,reject)=>{const request=indexedDB.open(name);request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
        const rows=await new Promise(resolve=>{const request=db.transaction('drafts').objectStore('drafts').getAll();request.onsuccess=()=>resolve(request.result);});
        const result={starting,message,version:db.version,unchanged:JSON.stringify(rows)===JSON.stringify([first,invalid])};db.close();return result;
      },starting);
      assert.match(result.message,starting?/Invalid starting draft.*saved drafts remain on this device/i:/Draft text must be a string.*saved drafts remain on this device/i);
      assert.equal(result.version,starting?1:7);assert(result.unchanged);reports.push(result);
    } finally {await context.close();}
  }
  const context=await browser.newContext();
  await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:files.get(new URL(route.request().url()).pathname.slice(1))??''}));
  try {
    const old=await context.newPage(),current=await context.newPage();
    await old.goto('http://localhost:37918/');await current.goto('http://localhost:37918/');
    const record=await old.evaluate(async()=>{
      const {openDraftDatabase}=await import('/draft-databases.js');
      const record={revision:7,incarnation:crypto.randomUUID(),text:'saved input',workspace:'workspace'};
      await openDraftDatabase('rsi.composer-starts',2,db=>{db.createObjectStore('drafts').put(record,JSON.stringify(['endpoint','local','main']));db.createObjectStore('usage').put({counts:[1,11],epoch:4},'aggregate');});return record;
    });
    assert.deepEqual(await current.evaluate(async()=>{const {StartingDraft}=await import('/starting-draft.js');return (await StartingDraft.load(['endpoint','local'],'main')).record;}),record);
    const retired=await old.evaluate(async()=>{try{(await import('/draft-databases.js')).assertDraftStorageActive();return false;}catch{return true;}});
    assert(retired);reports.push({schema2IdentityPreserved:true,olderDocumentRetired:true});
  } finally {await context.close();}
  return reports;
}

export async function verifyComposerWriterFence(browser, root, version = 9) {
  const files=new Map(await Promise.all(['drafts.js','draft-databases.js'].map(async name=>[name,await readFile(join(root,'apps/web',name),'utf8')])));
  const context=await browser.newContext();
  await context.route('http://localhost:37918/**',route=>route.fulfill({contentType:'text/javascript',body:files.get(new URL(route.request().url()).pathname.slice(1))??''}));
  try {
    const old=await context.newPage(),current=await context.newPage();
    await old.goto('http://localhost:37918/');await current.goto('http://localhost:37918/');
    const before=await old.evaluate(async version=>{
      const {DraftStore}=await import('/drafts.js'),store=new DraftStore(undefined,'a'.repeat(32),'local');
      const record={...store.blank('main','session','b'.repeat(64)),version,text:'retained',editRevision:3,
        pending:{kind:'command',id:'original-command',opaque:'opaque',text_bytes:0,images:0,captures:[],phase:'unknown',editRevision:3,preserve_editor:true}};
      if(version<9) record.scope=record.key.slice(0,3);
      await new Promise((resolve,reject)=>{
        const request=indexedDB.open('rsi.composer',version);
        request.onupgradeneeded=()=>{const records=request.result.createObjectStore('drafts',{keyPath:'key'});if(version<9) records.createIndex('scope','scope');records.put(record);request.result.createObjectStore('usage').put([1,8,0,6],'aggregate');};
        request.onsuccess=()=>{window.oldDb=request.result;oldDb.onversionchange=()=>{window.versionChanged=true;oldDb.close();};resolve();};request.onerror=()=>reject(request.error);
      });return record;
    },version);
    const after=await current.evaluate(async()=>{
      const {DraftStore}=await import('/drafts.js'),store=await DraftStore.open('a'.repeat(32),{kind:'local'});
      return store.get('main','session');
    });
    const {scope,...retained}=before;
    assert.deepEqual(after,{...retained,version:10});
    const oldWriter=await old.evaluate(async version=>({versionChanged:window.versionChanged,reopen:await new Promise(resolve=>{
      const request=indexedDB.open('rsi.composer',version);
      request.onerror=()=>resolve(request.error.name);request.onsuccess=()=>{request.result.close();resolve('opened');};
    })}),version);
    assert.deepEqual(oldWriter,{versionChanged:true,reopen:'VersionError'});
    return {pendingIdentityPreserved:true,recordUpgraded:true,...oldWriter};
  } finally {await context.close();}
}
