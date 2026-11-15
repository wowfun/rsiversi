import {DraftStore as OwnershipStore, DraftEditor as OwnershipEditor, captureOf, captureKey} from '/drafts.js';
import {createPaneClass} from '/src/runtime/session-controller.js';

export async function verifySettlementCapacity() {
  const store = await OwnershipStore.open('d'.repeat(32), {kind:'local'});
  for (const [settle, retired] of [[true,8],[false,8],[true,12],[false,12]]) {
    const id = `settlement-${crypto.randomUUID()}`, header = 'e'.repeat(64);
    let record = await store.ensure('main',id,header);
    const capture = index => ({target:{session_id:id,header_sha256:header},
      snapshot:{capture_id:`capture-${index}`,sha256:'a'.repeat(64),byte_len:900}});
    for(let index=0;index<retired;index++) record=await store.retireUnowned(record,[capture(index)]);
    const references=Array.from({length:4},(_,i)=>({snapshot:capture(i+retired).snapshot,preview:'Reference',metadata:{
      target:capture(i+retired).target,source:{kind:'native',binding:{session_id:'source',header_sha256:'b'.repeat(64)}},
      capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:'c'.repeat(64),scanned_bytes:100,omissions:[]}},text_bytes:9}}));
    record=await store.edit(record,'submit at capacity',[],references);
    record=await store.freeze(record,{kind:'message',id:'capacity',opaque:'frozen',text_bytes:18,images:0,captures:references.map(captureOf)});
    if(settle) {
      record=await store.begin(record);
      record=await store.settle(record,{status:'complete',receipt:'accepted'});
    } else {
      record=await store.edit(record,'new draft',[],[]);
      record=await store.cancelPrepared(record);
    }
    const saved=await store.get('main',id);
    if(saved.pending || saved.releases.length!==retired+4 || saved.text!==(settle?'':'new draft')) throw Error('settlement lost ownership or editor state');
  }
  await store.verify();
  return {completeAtCapacity:true,cancelAtCapacity:true};
}

// Runs in a real document/IndexedDB; the remote release boundary is fault injected.
export async function verifyReferenceOwnership() {
  const store = await OwnershipStore.open('d'.repeat(32), {kind:'local'});
  const id = `ownership-${crypto.randomUUID()}`, header = 'e'.repeat(64);
  const reference = {snapshot:{capture_id:`capture-${crypto.randomUUID()}`,sha256:'a'.repeat(64),byte_len:900},preview:'Reference',metadata:{
    source:{kind:'native',binding:{session_id:'source',header_sha256:'b'.repeat(64)}},target:{session_id:id,header_sha256:header},
    capture:{kind:'suffix',interval:{through_seq:'1',scanned_after_seq:'0',retained_after_seq:'0',retained_through_seq:'1',fact_prefix_sha256:'c'.repeat(64),scanned_bytes:100,omissions:[]}},text_bytes:9}};
  const calls = []; let offline = false;
  const context = {connection:{drafts:store},boundCall:()=>async(_method, text)=>{
    if (offline) throw Error('offline'); calls.push(JSON.parse(text).operation);
  }};
  const Pane = createPaneClass(context);
  const makePane = editor => Object.assign(Object.create(Pane.prototype), {index:'main',generation:'test',editor,
    input:{focus(){}},renderComposer(){},edit(...args){editor.edit(...args)}});
  const check = (value, label) => { if (!value) throw Error(label); };
  const editor = new OwnershipEditor(store, await store.ensure('main',id,header));
  editor.edit('saved',[],[reference]); await editor.flush();
  const pane = makePane(editor), edit = store.edit.bind(store);
  store.edit = async()=>{throw Error('disk full')};
  await pane.removeReference(captureKey(captureOf(reference))).catch(()=>{});
  check(calls.length === 0 && (await store.get('main',id)).references.length === 1, 'failed save released persisted owner');
  store.edit = edit; await editor.resolve(true);
  const frozen = {kind:'message',id:'request',opaque:'opaque exact request',text_bytes:5,images:0,captures:[{snapshot:{byte_len:reference.snapshot.byte_len,sha256:reference.snapshot.sha256,capture_id:reference.snapshot.capture_id},target:{header_sha256:header,session_id:id}}]};
  await editor.update(record => store.freeze(record,frozen));
  await pane.removeReference(captureKey(captureOf(reference)));
  check(calls.length === 0 && editor.record.pending.captures.length === 1 && editor.record.releases.length === 0, 'frozen owner released');
  await editor.update(record=>store.settle(record,{status:'unknown'}));
  await pane.drainReferenceReleases(editor,'test');
  check(calls.length === 0, 'unknown request released');
  await editor.update(record=>store.settle(record,{status:'complete',receipt:'exact receipt'}));
  offline = true;
  await pane.drainReferenceReleases(editor,'test').catch(()=>{});
  check(editor.record.releases.length === 1, 'offline outbox lost');
  offline = false;
  const reopened = new OwnershipEditor(store, await store.get('main',id));
  await makePane(reopened).drainReferenceReleases(reopened,'reopened');
  check(calls.length === 1 && reopened.record.releases.length === 0, 'reload did not release exactly one identity');
  check(Object.entries(reference.snapshot).every(([key,value])=>calls[0].snapshot[key]===value), 'release changed capture identity');

  const second = {...reference,snapshot:{...reference.snapshot,capture_id:'preparing-capture'}};
  reopened.edit('prepare',[],[second]); await reopened.flush();
  const stale = reopened.record;
  const other = await store.edit(stale,'other tab',[],[]);
  await store.freeze(stale,{...frozen,captures:[captureOf(second)]}).then(()=>{throw Error('removed capture was frozen')}, error=>check(error.code==='conflict','unexpected freeze failure'));
  reopened.edit('stale replacement'); await reopened.flush().catch(()=>{});
  await reopened.resolve(false).then(()=>{throw Error('retired capture resurrected')}, error=>check(/removed in another tab/.test(error.message),'unexpected replacement failure'));
  const recovered = new OwnershipEditor(store,other);
  await makePane(recovered).drainReferenceReleases(recovered,'second');

  // Hold the save queue so adding and removing a capture coalesce into one edit.
  let resume;
  recovered.enqueue(()=>new Promise(resolve=>{resume=resolve}));
  await Promise.resolve();
  const transient = {...reference,snapshot:{...reference.snapshot,capture_id:'never-persisted'}};
  recovered.edit('',[],[transient]);
  const removed = makePane(recovered).removeReference(captureKey(captureOf(transient)));
  check(!(await store.get('main',id)).references.length, 'transient capture was persisted before removal');
  const callsBefore = calls.length;
  resume(); await removed;
  check(calls.length===callsBefore+1 && calls.at(-1).snapshot.capture_id===transient.snapshot.capture_id, 'never-persisted capture leaked');
  check(!recovered.record.releases.length, 'transient release was not acknowledged');

  for (let index=0;index<16;index++) {
    const item = {...reference,snapshot:{...reference.snapshot,capture_id:`bounded-${index}`}};
    await recovered.update(record=>store.edit(record,'',[],[item]));
    await recovered.update(record=>store.edit(record,'',[],[]));
  }
  offline = true;
  await recovered.update(record=>store.edit(record,'offline text at capacity',[],[]));
  check(recovered.record.text==='offline text at capacity' && recovered.record.releases.length===16, 'full outbox blocked text edits');
  const full = recovered.record;
  await store.edit(full,'',[],[reference]).then(()=>{throw Error('unreserved reference admitted')},error=>check(/cleanup is pending/.test(error.message),'unexpected capture admission failure'));
  await store.retireUnowned(full,[{...captureOf(reference),snapshot:{...reference.snapshot,capture_id:'overflow'}}]).then(()=>{throw Error('outbox overflow accepted')},()=>{});
  const afterOverflow = await store.get('main',id);
  check(afterOverflow.releases.length===16 && afterOverflow.text===full.text, 'outbox overflow mutated saved state');
  offline = false;
  await makePane(recovered).drainReferenceReleases(recovered,'bounded');
  const references = recovered.references, encode = TextEncoder.prototype.encode;
  let largeEncodes = 0;
  TextEncoder.prototype.encode = function(text) { if (text?.length >= 900000) largeEncodes++; return encode.call(this,text); };
  try { recovered.edit('x'.repeat(900000)); } finally { TextEncoder.prototype.encode = encode; }
  check(largeEncodes === 1 && recovered.references === references, 'keystroke repeated encoding or attachment identity');
  await recovered.flush(); await store.verify();
  return {failedSave:true,frozenOwner:true,unknownOwner:true,reloadedOutbox:true,preparationConflict:true,staleReplacement:true,transientRelease:true,offlineTextAtCapacity:true,boundedOutbox:true,largeEncodes};
}
