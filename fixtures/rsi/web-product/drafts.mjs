import assert from "node:assert/strict";
import {serveDraftModules} from './draft-origin.mjs';

// Real IndexedDB transactions in two documents; no Session/provider claims.
export async function verifyDrafts(browser, root) {
  const context = await browser.newContext();
  await serveDraftModules(context,root,37919);
  const page = await context.newPage(), other = await context.newPage();
  try {
    for (const tab of [page, other]) {
      await tab.goto("http://localhost:37919/");
      await tab.evaluate(async () => {
        const { DraftStore, DraftEditor } = await import("/drafts.js");
        Object.assign(window, { DraftStore, DraftEditor, store: await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)}) });
      });
    }
    await page.evaluate(async () => {
      window.record = await store.ensure("main", "first", "c".repeat(64));
      record = await store.edit(record, "original", []);
    });
    await other.evaluate(async () => { window.stale = await store.get("main", "first"); });
    await page.evaluate(async () => { record = await store.edit(record, "new saved text", []); });
    await other.evaluate(() => store.edit(stale, "last window edit", []));
    assert.equal(await page.evaluate(async () => (await store.get("main","first")).text), "last window edit");
    const outcomes = await page.evaluate(async () => {
      const captured = record;
      // A u64 larger than JS's safe integer stays an opaque Rust string.
      const opaque = '{"revision":18446744073709551615,"arguments":{"z":1,"a":2}}';
      record = await store.edit(record, "typed during preparation", []);
      record = await store.startDispatch(captured, { kind: "command", id: "original-id", opaque, text_bytes: 0, images: 0, captures: [] });
      window.prepared = await store.settle(record, {status:'not_admitted'});
      return { text: record.text, captured: record.pending.editRevision, current: record.editRevision };
    });
    assert(outcomes.captured < outcomes.current);
    await other.evaluate(async () => { window.pending = await store.get("main", "first"); });
    await page.evaluate(async () => { window.dispatched = await store.claimPrepared(prepared); });
    assert.equal(await other.evaluate(async () => {
      try { await store.claimPrepared(pending); return "dispatched twice"; } catch (error) { return error.code; }
    }), "conflict");
    const settled = await page.evaluate(async () => {
      const committed = await store.get("main", "first");
      if (committed.pending.phase !== "dispatching") throw new Error("begin resolved before commit");
      const editor = new DraftEditor(store, dispatched);
      editor.edit("typed during execution"); await editor.flush();
      await editor.settle(dispatched, {status:"complete"});
      window.record = editor.record;
      return { text: editor.text, pending: editor.record.pending, opaque: dispatched.pending.opaque,
        hasReceipt: Object.hasOwn(await store.get("main","first"),"receipt") };
    });
    assert.equal(settled.text, "typed during execution");
    assert.equal(settled.pending, null);
    assert.equal(settled.opaque, '{"revision":18446744073709551615,"arguments":{"z":1,"a":2}}');
    assert.equal(settled.hasReceipt, false);
    const queued = await page.evaluate(async () => {
      const original = await store.get("main", "first");
      const opaque = '{"session":"first","header":"frozen-header","request":{"kind":"queue","input":{"operation_id":"queue-original","slot_id":"slot","expected_message_id":"old","mutation":{"action":"replace","new_message_id":"successor","content":[{"type":"text","text":"complete queued text"}]}}}}';
      const prepared = await store.startDispatch(original, {kind:"queue",id:"queue-original",opaque,text_bytes:20,images:0,captures: []});
      const unknown = await store.settle(prepared, {status:"unknown"});
      const reopened = await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)});
      const restored = await reopened.get("main","first");
      if(restored.pending.opaque !== opaque || restored.pending.id !== "queue-original") throw new Error("unknown queue envelope changed on reopen");
      let cancelled=false;try {await reopened.cancelPrepared(restored);}catch {cancelled=true;}
      const finished = await reopened.settle(restored, {status:"complete"});
      return {cancelled,kind:unknown.pending.kind,phase:restored.pending.phase,text:finished.text,images:finished.images,pending:finished.pending};
    });
    assert.deepEqual(queued,{cancelled:true,kind:"queue",phase:"unknown",text:"typed during execution",images:[],pending:null});
    assert.deepEqual(await page.evaluate(async()=>{
      const original=await store.get("main","first");
      const prepared=await store.startDispatch(original,{kind:"queue",id:"conflict",opaque:'{"operation":"conflict"}',text_bytes:0,images:0,captures: []});
      const finished=await store.settle(prepared,{status:"rejected",error:"identity conflict"});
      return {text:finished.text,pending:finished.pending,hasReceipt:Object.hasOwn(finished,"receipt")};
    }),{text:"typed during execution",pending:null,hasReceipt:false});
    const boundaries = await page.evaluate(async () => {
      let record = await store.ensure("compare", "aba", "c".repeat(64));
      record = await store.edit(record, "", []);
      await store.removeEmpty(record);
      const replacement = await store.edit(await store.ensure("compare", "aba", "c".repeat(64)), "", []);
      if ((await store.get("compare", "aba")).incarnation !== replacement.incarnation) throw Error("replacement was not durable");
      let aba = false;
      try { await store.edit(record, "late upload", []); } catch (error) { aba = error.code === "conflict"; }
      let a = await store.ensure("compare", "full-a", "c".repeat(64));
      a = await store.edit(a, "x".repeat(1024 * 1024), []);
      const second = await DraftStore.open("d".repeat(32), {kind:"device",device_id:"e".repeat(32)});
      let b = await second.ensure("compare", "full-b", "c".repeat(64));
      b = await second.edit(b, "x".repeat(1024 * 1024), []);
      let c = await second.ensure("third", "full-c", "c".repeat(64));
      await second.edit(c, "x".repeat(1024 * 1024), []);
      const originalBytes = new TextEncoder().encode((await store.get("main", "first")).text).length;
      let d = await second.ensure("fourth", "full-d", "c".repeat(64));
      await second.edit(d, "x".repeat(1024 * 1024 - originalBytes), []);
      await store.edit(replacement, "over four MiB across Sessions", []);
      return { aba, restored:(await store.get("compare","aba")).text, otherDevice: (await second.list("main")).rows.length, originalDevice: (await store.list("main")).rows.length };
    });
    assert.deepEqual(boundaries, { aba: true, restored:"over four MiB across Sessions", otherDevice: 0, originalDevice: 1 });
    const aborted = await page.evaluate(async () => {
      const captured = await store.get("main", "first");
      const transaction = store.db.transaction.bind(store.db);
      store.db.transaction = (...args) => {
        const tx = transaction(...args);
        if (args[1] === "readwrite") queueMicrotask(() => tx.abort());
        return tx;
      };
      const editor = new DraftEditor(store, captured);
      editor.edit("unsaved survives quota/abort");
      let failed = false;
      try { await editor.flush(); } catch { failed = true; }
      store.db.transaction = transaction;
      let transferred = false, transferRejected = false;
      try { await editor.transfer(async () => { transferred = true; }); } catch { transferRejected = true; }
      return { failed, transferRejected, transferred, unlocked: !editor.transferring, text: editor.text, durable: (await store.get("main", "first")).text, pending: (await store.get("main", "first")).pending };
    });
    assert.deepEqual(aborted, { failed: true, transferRejected: true, transferred: false, unlocked: true, text: "unsaved survives quota/abort", durable: "typed during execution", pending: null });
    await page.reload();
    assert.equal(await page.evaluate(async () => {
      const { DraftStore } = await import("/drafts.js");
      return (await (await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)})).get("main", "first")).text;
    }), "typed during execution");
    const more = await page.evaluate(async () => {
      const { DraftStore, DraftEditor } = await import("/drafts.js");
      const store = await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)});
      const clear = () => new Promise((resolve, reject) => {
        const tx = store.db.transaction(["drafts", "usage"], "readwrite");
        tx.objectStore("drafts").clear(); tx.objectStore("usage").clear();
        tx.oncomplete = resolve; tx.onabort = () => reject(tx.error);
      });
      const rejects = async operation => { try { await operation(); return false; } catch { return true; } };
      await clear();
      const creation = session_id => ({session_id, workspace_id:"a".repeat(64), agent_preset_id:null});
      let original = await store.ensure("main", "fresh-old", "c".repeat(64), creation("fresh-old"));
      original = await store.edit(original, "keep this input", [{id:"d".repeat(64),mime:"image/png",bytes:72,width:1,height:1}]);
      const replacement = await store.ensure("main", "fresh-new", "c".repeat(64), creation("fresh-new"));
      const moved = await store.moveFresh(original, replacement);
      const freshMove = !await store.get("main", "fresh-old") && moved.text === original.text && moved.images[0].id === original.images[0].id;
      const pending = await store.startDispatch(moved, {kind:"command",id:"same-command",opaque:"{}",text_bytes:0,images: 0, captures: []});
      const next = await store.ensure("main", "fresh-forbidden", "c".repeat(64), creation("fresh-forbidden"));
      const noReplay = await rejects(() => store.moveFresh(pending, next)) && await rejects(() => store.cancelPrepared(pending));
      const transaction = store.db.transaction.bind(store.db);
      store.db.transaction = (...args) => { const tx = transaction(...args); if (args[1] === "readwrite") queueMicrotask(() => tx.abort()); return tx; };
      const settlementFailure = await rejects(() => store.settle(pending, {status:"complete"}));
      store.db.transaction = transaction;
      const stillPending = (await store.get("main","fresh-new")).pending;
      const settlementBlocked = settlementFailure && stillPending.phase === "dispatching" && stillPending.id === "same-command" && stillPending.opaque === pending.pending.opaque && await rejects(() => store.startDispatch(pending,{kind:"message",id:"new-id",opaque:"{}",text_bytes:0,images: 0, captures: []}));
      const invalidSettlement = await rejects(() => store.settle(pending, {status:"invalid"})) &&
        (await store.get("main","fresh-new")).pendingRevision === pending.pendingRevision;
      const notAdmitted = await store.settle(pending, {status:"not_admitted"});
      const cancelled = await store.cancelPrepared(notAdmitted);
      const conservativeMarker = cancelled.everDispatched && !cancelled.pending && await rejects(() => store.moveFresh(cancelled, next));
      await clear();
      const editor = new DraftEditor(store, await store.ensure("main","transferring","c".repeat(64)));
      editor.edit("latest local input");
      let started, release;
      const entered = new Promise(resolve => { started=resolve; });
      const held = new Promise(resolve => { release=resolve; });
      let captured;
      const moving = editor.transfer(async record => { captured=record.text; started(); await held; });
      await entered;
      const transferLocked = await rejects(() => editor.edit("late edit")) && await rejects(() => editor.update(record=>store.edit(record,"late update",[]))) && await rejects(() => editor.resolve(true)) && await rejects(() => editor.transfer(async()=>{}));
      release(); await moving;
      editor.edit("after transfer"); await editor.flush();
      const transferSaved = captured === "latest local input" && editor.record.text === "after transfer";
      let reading, finishRead;
      const readEntered = new Promise(resolve=>{reading=resolve;});
      const readHeld = new Promise(resolve=>{finishRead=resolve;});
      const get = store.get.bind(store);
      store.get = async (...args) => {reading(); await readHeld; return get(...args);};
      const resolving = editor.resolve(false);
      await readEntered;
      const resolvingBlocksTransfer = await rejects(() => editor.transfer(async()=>{}));
      finishRead(); await resolving; store.get=get;
      await editor.transfer(async record=>{if(record.text!=="after transfer") throw new Error("resolved input changed");});
      await clear();
      for (let index=0; index<128; index++) await store.edit(await store.ensure("main", `record-${index}`, "c".repeat(64)), "retained input", []);
      await store.edit(await store.ensure("main","record-129","c".repeat(64)), "another note", []);
      const extraRestored = (await store.get("main","record-129")).text === "another note" && (await store.get("main","record-0")).text === "retained input";
      await clear();
      let a=await store.ensure("main","frozen-a","c".repeat(64)), b=await store.ensure("main","frozen-b","c".repeat(64)), c=await store.ensure("main","frozen-c","c".repeat(64));
      const freeze = (record, length, text_bytes=0) => store.startDispatch(record,{kind:"message",id:record.key[3],opaque:"x".repeat(length),text_bytes,images: 0, captures: []});
      a=await freeze(a,8*1024*1024,1024*1024); b=await freeze(b,8*1024*1024,1024*1024);
      a=await store.settle(a,{status:"unknown"}); b=await store.settle(b,{status:"unknown"});
      let extra = await store.ensure("dynamic", "frozen-extra", "c".repeat(64));
      await freeze(extra,8*1024*1024,1024*1024);
      extra = await store.ensure("another", "frozen-extra-two", "c".repeat(64));
      await freeze(extra,8*1024*1024,1024*1024);
      const frozenBound = await rejects(() => freeze(c,1)) && await rejects(() => freeze(c,0,1)) && await rejects(()=>store.cancelPrepared(a)) && await rejects(()=>store.removeEmpty(b));
      await store.verify();
      // Corrupt the isolated durable fixture, preserving valid records and detecting counters.
      await new Promise((resolve,reject) => { const tx=store.db.transaction("usage","readwrite"); tx.objectStore("usage").put([0,0,0,0],"aggregate"); tx.oncomplete=resolve; tx.onabort=()=>reject(tx.error); });
      const corruptRejected = await rejects(() => DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)}));
      const unchanged = (await store.get("main","frozen-a")).pending.opaque.length === 8*1024*1024;
      await clear();
      const malformed=await store.ensure("main","malformed","c".repeat(64));
      const {key,...invalid}=malformed;
      invalid.pending=false;
      await new Promise((resolve,reject) => {const tx=store.db.transaction("drafts","readwrite");tx.objectStore("drafts").put(invalid,[...store.storedNamespace,key[2],key[3]]);tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error);});
      const invalidPending = await rejects(() => store.get("main","malformed")) && await rejects(() => store.list("main"));
      return {freshMove,noReplay,settlementBlocked,invalidSettlement,conservativeMarker,transferLocked,transferSaved,resolvingBlocksTransfer,extraRestored,frozenBound,corruptRejected,unchanged,invalidPending};
    });
    assert(Object.values(more).every(value => value === true), JSON.stringify(more));
    return { two_documents: true, single_dispatch: true, opaque_u64: true, host_only_receipt: true, edited_suffix: true, aba: true, cross_namespace_bounds: true, transaction_abort: true, reload: true, ...more };
  } finally { await context.close(); }
}
