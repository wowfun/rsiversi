import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

export async function verifyDraftReferences(browser, root) {
  const source = await readFile(join(root, 'plugins/rsi/web/drafts.js'), 'utf8');
  const context = await browser.newContext();
  await context.route('http://localhost:37918/**', route => route.fulfill({contentType:'text/javascript',body:route.request().url().endsWith('drafts.js') ? source : ''}));
  const page = await context.newPage();
  try {
    await page.goto('http://localhost:37918/');
    const result = await page.evaluate(async () => {
      const {DraftStore,DraftEditor,validateEditor} = await import('/drafts.js');
      const store = await DraftStore.open('a'.repeat(32),{kind:'local'});
      const reference = {snapshot:{sha256:'b'.repeat(64),byte_len:900},metadata:{source:{session_id:'source',header_sha256:'c'.repeat(64)},target:{session_id:'target',header_sha256:'d'.repeat(64)},through_seq:'9007199254740993',fact_prefix_sha256:'e'.repeat(64),scanned_after_seq:'9007199254740992',retained_after_seq:'9007199254740992',retained_through_seq:'9007199254740993',scanned_bytes:400,text_bytes:12,omissions:['fact_limit']},preview:'你好世界'};
      const creation = id => ({session_id:id,workspace_id:'f'.repeat(64),agent_preset_id:null});
      let record = await store.ensure('main','target','d'.repeat(64),creation('target'));
      const editor = new DraftEditor(store,record);
      const beforeReferences = editor.referencesRevision;
      editor.edit('Draft',[],[reference]); await editor.flush();
      if (editor.referencesRevision <= beforeReferences) throw new Error('Reference edits must advance presentation identity');
      const restored = new DraftEditor(store,await store.get('main','target'));
      const persistent = JSON.stringify(restored.references) === JSON.stringify([reference]);
      const replacement = await store.ensure('main','replacement','c'.repeat(64),creation('replacement'));
      let refusedMove = false, refusedBinding = false;
      try {await store.moveFresh(editor.record,replacement)} catch(error) {refusedMove = error.message.includes('original conversation')}
      try {await store.edit(replacement,'bad',[],[reference])} catch(error) {refusedBinding = error.message.includes('another draft Header')}
      record = await store.get('main','target');
      const original = JSON.stringify({large:'18446744073709551615',references:[reference]});
      record = await store.freeze(record,{kind:'message',id:'stable-input',opaque:original,text_bytes:17,images:0,references:1});
      record = await store.begin(record);
      record = await store.edit(record,'Next draft',[],[]);
      const frozen = record.pending.opaque === original && record.pending.references === 1;
      record = await store.settle(record,{status:'unknown'});
      const retry = record.pending.opaque === original && record.pending.phase === 'unknown';
      record = await store.settle(record,{status:'complete',receipt:'{"fact_seq":"18446744073709551615"}'});
      const laterDraft = record.text === 'Next draft' && record.references.length === 0 && !record.pending;
      let bounded = 0;
      for (const invalid of [()=>validateEditor('x'.repeat(1024*1024),[],[reference]),()=>validateEditor('',[],Array(5).fill(reference)),()=>validateEditor('',[],[{...reference,metadata:{...reference.metadata,through_seq:9007199254740993}}])]) {try {invalid()}catch {bounded++}}
      await store.verify();
      return {persistent,refusedMove,refusedBinding,frozen,retry,laterDraft,bounded};
    });
    assert.deepEqual(result,{persistent:true,refusedMove:true,refusedBinding:true,frozen:true,retry:true,laterDraft:true,bounded:3});
    return result;
  } finally {await context.close()}
}
