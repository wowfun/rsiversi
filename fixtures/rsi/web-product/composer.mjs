import assert from "node:assert/strict";

// Actual document methods and IndexedDB, with gated persistence and Worker replies.
export async function verifyComposer(page) {
  const switching = await page.evaluate(async () => {
    const pane = panes.get("main"), store = connection.drafts;
    renderDetail({ detail: null });
    const snapshot = (session, generation = session) => ({ generation, session, header: "c".repeat(64), path: "/workspace",
      model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" });
    pane.render(snapshot("saved-switch"), []); await pane.binding;
    pane.edit("Keep this draft while switching"); await pane.flush();
    pane.render(snapshot("other-switch"), []); await pane.binding;
    const ensure = store.ensure;
    let release;
    const held = new Promise(resolve => { release = resolve; });
    store.ensure = async function (...args) { await held; return ensure.apply(this, args); };
    window.releaseDraftSwitch = async () => {
      release();
      try { await pane.binding; } finally { store.ensure = ensure; delete window.releaseDraftSwitch; }
    };
    pane.render(snapshot("saved-switch", "restored-switch"), []);
    return { header: pane.session.textContent, disabled: pane.input.disabled, text: pane.input.value };
  });
  assert.deepEqual(switching, { header: "/workspace · saved-switch", disabled: true, text: "" });
  const restored = page.getByRole("textbox", { name: "Main message", exact: true });
  try {
    const actionable = restored.click({ trial: true });
    await page.evaluate(() => window.releaseDraftSwitch());
    await actionable;
    assert.equal(await restored.inputValue(), "Keep this draft while switching");
  } finally { await page.evaluate(() => window.releaseDraftSwitch?.()); }
  const races = await page.evaluate(async () => {
    const pane = panes.get("main"), originalCall = call, results = [];
    try {
      for (const [firstMode, secondMode] of [["submit", "submit"], ["query", "query"], ["submit", "query"], ["query", "submit"]]) {
        const session = `overlap-${firstMode}-${secondMode}`;
        pane.render({ generation: session, session, path: "/workspace", model: { deployment: "test", model: "model" },
          transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" }, []);
        await pane.binding;
        pane.edit("one saved input"); await pane.flush();
        const editor = pane.editor;
        if (firstMode === "query" || secondMode === "query") {
          await editor.update(record => editor.store.freeze(record, { kind: "message", id: session, opaque: "{}", text_bytes: 15, images: 0 }));
          await editor.update(record => editor.store.begin(record));
        }
        let entered, release, preparations = 0, dispatches = 0;
        const waiting = new Promise(resolve => { entered = resolve; });
        const held = new Promise(resolve => { release = resolve; });
        const flush = editor.flush.bind(editor);
        editor.flush = async () => { entered(); await held; await flush(); };
        call = async method => {
          if (method === "prepare_submission") {
            preparations++;
            return JSON.stringify({ kind: "message", id: session, opaque: "{}", text_bytes: 15, images: 0 });
          }
          dispatches++;
          return JSON.stringify({ status: "complete", receipt: "{}" });
        };
        const invoke = mode => mode === "submit" ? pane.submit(false) : pane.reconcile("query");
        const first = invoke(firstMode);
        await waiting;
        const busyWhileSaving = pane.submitting && pane.send.disabled;
        const second = invoke(secondMode);
        release();
        const settled = await Promise.allSettled([first, second]);
        editor.flush = flush;
        results.push({ firstMode, secondMode, busyWhileSaving, preparations, dispatches,
          errors: settled.filter(value => value.status === "rejected").map(value => value.reason.message),
          failure: editor.failure?.message ?? null, pending: editor.record.pending, unlocked: !pane.submitting });
      }
      return results;
    } finally { call = originalCall; }
  });
  for (const result of races) assert.deepEqual(result, {
    firstMode: result.firstMode, secondMode: result.secondMode, busyWhileSaving: true,
    preparations: result.firstMode === "submit" && result.secondMode === "submit" ? 1 : 0,
    dispatches: 1, errors: [], failure: null, pending: null, unlocked: true,
  });
  const recovery = await page.evaluate(async () => {
    const pane = panes.get("main"), originalCall = call, originalReconcile = pane.reconcile;
    const originalFlush = DraftEditor.prototype.flush;
    let release, entered, automatic, dispatches = 0;
    const held = new Promise(resolve => { release = resolve; });
    const waiting = new Promise(resolve => { entered = resolve; });
    try {
      const store = connection.drafts, session = "automatic-reconcile-overlap";
      let record = await store.ensure("main", session, "c".repeat(64));
      record = await store.freeze(record, { kind: "message", id: session, opaque: "{}", text_bytes: 0, images: 0 });
      await store.begin(record);
      DraftEditor.prototype.flush = async function () { entered(); await held; return originalFlush.call(this); };
      pane.reconcile = function (mode) { const result = originalReconcile.call(this, mode); automatic ??= result; return result; };
      call = async () => { dispatches++; return JSON.stringify({ status: "complete", receipt: "{}" }); };
      pane.render({ generation: session, session, path: "/workspace", model: { deployment: "test", model: "model" },
        transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" }, []);
      await pane.binding; await waiting;
      const manual = pane.reconcile("query");
      release(); await Promise.all([automatic, manual]);
      DraftEditor.prototype.flush = originalFlush;
      const failure = pane.editor.failure?.message ?? null, pending = pane.editor.record.pending;
      const failures = [];
      pane.editor.flush = async () => { throw new Error("injected save failure"); };
      for (const mode of ["submit", "query"]) {
        let error;
        try { await (mode === "submit" ? pane.submit(false) : pane.reconcile("query")); }
        catch (reason) { error = reason.message; }
        failures.push({ error, unlocked: !pane.submitting });
      }
      delete pane.editor.flush;
      return { dispatches, failure, pending, failures };
    } finally { release(); call = originalCall; pane.reconcile = originalReconcile; DraftEditor.prototype.flush = originalFlush; }
  });
  assert.deepEqual(recovery, { dispatches: 1, failure: null, pending: null,
    failures: [{ error: "injected save failure", unlocked: true }, { error: "injected save failure", unlocked: true }] });
  const accounting = await page.evaluate(async () => {
    const pane = panes.get("main"), store = connection.drafts;
    const sibling = new DraftEditor(store, { ...store.blank("main", "inactive-editor", "c".repeat(64)), text: "界".repeat(4096) });
    pane.editors.set("inactive-editor", sibling);
    const encode = TextEncoder.prototype.encode;
    let inactiveEncodes = 0;
    try {
      TextEncoder.prototype.encode = function (value) {
        if (value === sibling.text) inactiveEncodes++;
        return encode.call(this, value);
      };
      pane.edit("active UTF-8 🦀");
    } finally { TextEncoder.prototype.encode = encode; }
    await pane.flush();
    // Imported/restored editor text uses the same accounting as a keystroke.
    sibling.text = "界".repeat(349525); // 1 MiB minus one byte.
    const second = new DraftEditor(store, { ...store.blank("main", "full-editor", "c".repeat(64)), text: "x".repeat(1024 * 1024) });
    pane.editors.set("full-editor", second);
    const previous = pane.editor.text;
    let bounded = false;
    try { pane.edit("🦀"); } catch (error) { bounded = error.message.includes("2 MiB"); }
    pane.editors.delete("inactive-editor"); pane.editors.delete("full-editor");
    return { inactiveEncodes, bounded, retained: pane.editor.text === previous };
  });
  assert.deepEqual(accounting, { inactiveEncodes: 0, bounded: true, retained: true });
  const close = await page.evaluate(async () => {
    const pane = panes.get("main"), store = connection.drafts;
    const inactive = new DraftEditor(store, await store.ensure("main", "failed-inactive", "c".repeat(64)));
    inactive.text = "unsaved inactive input"; inactive.dirty = true;
    inactive.failure = new Error("injected inactive save failure");
    const key = JSON.stringify(inactive.record.key);
    pane.editors.set(key, inactive);
    const active = pane.editor;
    await pane.flush();
    let error;
    try { await pane.flush(true); } catch (failure) { error = failure.message; }
    const preserved = pane.editor === active && pane.editors.get(key) === inactive && inactive.text === "unsaved inactive input";
    await inactive.resolve(false);
    await pane.flush(true);
    return {error, preserved, saved:(await store.get("main", "failed-inactive")).text};
  });
  assert.deepEqual(close, {error:"injected inactive save failure",preserved:true,saved:"unsaved inactive input"});
  return { header_before_draft: switching, restored_draft: true, overlapping_flows: races.length, automatic_reconciliation: true, failed_save_releases_admission: true, cached_utf8_accounting: true, inactive_close_recovery: true };
}
