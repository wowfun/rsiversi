import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

// Real IndexedDB transactions in two documents; no Session/provider claims.
export async function verifyDrafts(browser, root) {
  const context = await browser.newContext();
  const source = await readFile(join(root, "plugins/rsi/web/drafts.js"), "utf8");
  await context.route("http://localhost:37919/**", route => route.fulfill({ contentType: "text/javascript", body: route.request().url().endsWith("drafts.js") ? source : "" }));
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
    assert.equal(await other.evaluate(async () => {
      try { await store.edit(stale, "stale overwrite", []); return "overwritten"; }
      catch (error) { return error.code; }
    }), "conflict");
    const outcomes = await page.evaluate(async () => {
      const captured = record;
      // A u64 larger than JS's safe integer stays an opaque Rust string.
      const opaque = '{"revision":18446744073709551615,"arguments":{"z":1,"a":2}}';
      record = await store.edit(record, "typed during preparation", []);
      record = await store.freeze(captured, { kind: "command", id: "original-id", opaque, text_bytes: 0, images: 0 });
      window.prepared = record;
      return { text: record.text, captured: record.pending.editRevision, current: record.editRevision };
    });
    assert(outcomes.captured < outcomes.current);
    await other.evaluate(async () => { window.pending = await store.get("main", "first"); });
    await page.evaluate(async () => { window.dispatched = await store.begin(prepared); });
    assert.equal(await other.evaluate(async () => {
      try { await store.begin(pending); return "dispatched twice"; } catch (error) { return error.code; }
    }), "conflict");
    const settled = await page.evaluate(async () => {
      const committed = await store.get("main", "first");
      if (committed.pending.phase !== "dispatching") throw new Error("begin resolved before commit");
      const editor = new DraftEditor(store, dispatched);
      editor.edit("typed during execution"); await editor.flush();
      const receipt = '{"request_id":"original-id","accepted_control_seq":18446744073709551615,"observed_fact_seq":18446744073709551614}';
      await editor.update(() => store.settle(dispatched, JSON.parse(JSON.stringify({ status: "complete", receipt }))));
      window.record = editor.record;
      return { text: editor.text, pending: editor.record.pending, opaque: dispatched.pending.opaque, receipt: editor.record.receipt };
    });
    assert.equal(settled.text, "typed during execution");
    assert.equal(settled.pending, null);
    assert.equal(settled.opaque, '{"revision":18446744073709551615,"arguments":{"z":1,"a":2}}');
    assert.equal(settled.receipt, '{"request_id":"original-id","accepted_control_seq":18446744073709551615,"observed_fact_seq":18446744073709551614}');
    const boundaries = await page.evaluate(async () => {
      let record = await store.ensure("compare", "aba", "c".repeat(64));
      await store.removeEmpty(record);
      const replacement = await store.ensure("compare", "aba", "c".repeat(64));
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
      let bounded = false;
      try { await store.edit(replacement, "one byte too many", []); } catch { bounded = true; }
      return { aba, bounded, otherDevice: (await second.list("main")).length, originalDevice: (await store.list("main")).length };
    });
    assert.deepEqual(boundaries, { aba: true, bounded: true, otherDevice: 0, originalDevice: 1 });
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
      const creation = session_id => ({session_id, workspace_id:"a".repeat(64), workspace_trust:"untrusted", agent_preset_id:null});
      let original = await store.ensure("main", "fresh-old", "c".repeat(64), creation("fresh-old"));
      original = await store.edit(original, "keep this input", [{id:"d".repeat(64),mime:"image/png",bytes:72,width:1,height:1}]);
      const replacement = await store.ensure("main", "fresh-new", "c".repeat(64), creation("fresh-new"));
      const moved = await store.moveFresh(original, replacement);
      const freshMove = !await store.get("main", "fresh-old") && moved.text === original.text && moved.images[0].id === original.images[0].id;
      const prepared = await store.freeze(moved, {kind:"command",id:"same-command",opaque:"{}",text_bytes:0,images:0});
      const pending = await store.begin(prepared);
      const next = await store.ensure("main", "fresh-forbidden", "c".repeat(64), creation("fresh-forbidden"));
      const noReplay = await rejects(() => store.moveFresh(pending, next)) && await rejects(() => store.cancelPrepared(pending));
      const transaction = store.db.transaction.bind(store.db);
      store.db.transaction = (...args) => { const tx = transaction(...args); if (args[1] === "readwrite") queueMicrotask(() => tx.abort()); return tx; };
      const receiptFailure = await rejects(() => store.settle(pending, {status:"complete",receipt:'{"request_id":"same-command"}'}));
      store.db.transaction = transaction;
      const stillPending = (await store.get("main","fresh-new")).pending;
      const receiptBlocked = receiptFailure && stillPending.phase === "dispatching" && stillPending.id === "same-command" && await rejects(() => store.freeze(pending,{kind:"message",id:"new-id",opaque:"{}",text_bytes:0,images:0}));
      const invalidReceipt = await rejects(() => store.settle(pending, {status:"complete",receipt:{}})) &&
        await rejects(() => store.settle(pending, {status:"complete",receipt:"x".repeat(4097)})) &&
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
      for (let index=0; index<128; index++) await store.ensure("main", `record-${index}`, "c".repeat(64));
      const recordBound = await rejects(() => store.ensure("main","record-129","c".repeat(64))) && (await store.list("main")).length === 128;
      await clear();
      let a=await store.ensure("main","frozen-a","c".repeat(64)), b=await store.ensure("main","frozen-b","c".repeat(64)), c=await store.ensure("main","frozen-c","c".repeat(64));
      const freeze = (record, length, text_bytes=0) => store.freeze(record,{kind:"message",id:record.key[3],opaque:"x".repeat(length),text_bytes,images:0});
      a=await freeze(a,8*1024*1024,1024*1024); b=await freeze(b,8*1024*1024,1024*1024);
      a=await store.settle(await store.begin(a),{status:"unknown"}); b=await store.settle(await store.begin(b),{status:"unknown"});
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
      malformed.pending=false;
      await new Promise((resolve,reject) => {const tx=store.db.transaction("drafts","readwrite");tx.objectStore("drafts").put(malformed);tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error);});
      const invalidPending = await rejects(() => DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)}));
      return {freshMove,noReplay,receiptBlocked,invalidReceipt,conservativeMarker,transferLocked,transferSaved,resolvingBlocksTransfer,recordBound,frozenBound,corruptRejected,unchanged,invalidPending};
    });
    assert(Object.values(more).every(value => value === true), JSON.stringify(more));
    return { two_documents: true, single_dispatch: true, opaque_u64: true, opaque_receipt_u64: true, edited_suffix: true, aba: true, cross_namespace_bounds: true, transaction_abort: true, reload: true, ...more };
  } finally { await context.close(); }
}
