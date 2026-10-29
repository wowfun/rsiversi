import assert from "node:assert/strict";

// Actual document methods and IndexedDB, with gated persistence and Worker replies.
export async function verifyComposer(page) {
  const switching = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts;
    await fixture.surface("saved-switch", { path: "/workspace" });
    await pane.binding;
    pane.edit("Keep this draft while switching");
    await pane.flush();
    await fixture.surface("other-switch", { path: "/workspace" });
    await pane.binding;
    const ensure = store.ensure;
    let release;
    const held = new Promise((resolve) => {
      release = resolve;
    });
    store.ensure = async function (...args) {
      await held;
      return ensure.apply(this, args);
    };
    window.releaseDraftSwitch = async () => {
      release();
      try {
        await pane.binding;
      } finally {
        store.ensure = ensure;
        delete window.releaseDraftSwitch;
      }
    };
    await fixture.surface("saved-switch", {
      generation: "restored-switch",
      path: "/workspace",
    });
    return {
      header: pane.node.querySelector(".pane-session").textContent,
      disabled: pane.input.disabled,
      text: pane.input.value,
    };
  });
  assert.deepEqual(switching, {
    header: "/workspace · saved-switch",
    disabled: true,
    text: "",
  });
  const restored = page.getByRole("textbox", {
    name: "Main message",
    exact: true,
  });
  try {
    const actionable = restored.click({ trial: true });
    await page.evaluate(() => window.releaseDraftSwitch());
    await actionable;
    assert.equal(
      await restored.inputValue(),
      "Keep this draft while switching",
    );
  } finally {
    await page.evaluate(() => window.releaseDraftSwitch?.());
  }
  const races = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      results = [];
    try {
      for (const [firstMode, secondMode] of [
        ["submit", "submit"],
        ["query", "query"],
        ["submit", "query"],
        ["query", "submit"],
      ]) {
        const session = `overlap-${firstMode}-${secondMode}`;
        await fixture.surface(session);
        await pane.binding;
        pane.edit("one saved input");
        await pane.flush();
        const editor = pane.editor;
        if (firstMode === "query" || secondMode === "query") {
          await editor.update((record) =>
            editor.store.freeze(record, {
              kind: "message",
              id: session,
              opaque: "{}",
              text_bytes: 15,
              images: 0,
              references: 0,
            }),
          );
          await editor.update((record) => editor.store.begin(record));
        }
        let entered,
          release,
          preparations = 0,
          dispatches = 0;
        const waiting = new Promise((resolve) => {
          entered = resolve;
        });
        const held = new Promise((resolve) => {
          release = resolve;
        });
        const flush = editor.flush.bind(editor);
        editor.flush = async () => {
          entered();
          await held;
          await flush();
        };
        fixture.setReply("prepare_submission", async () => {
          preparations++;
          return JSON.stringify({
            kind: "message",
            id: session,
            opaque: "{}",
            text_bytes: 15,
            images: 0,
            references: 0,
          });
        });
        fixture.setReply("dispatch_submission", async () => {
          dispatches++;
          return JSON.stringify({ status: "complete", receipt: "{}" });
        });
        const invoke = (mode) =>
          mode === "submit" ? pane.submit("primary") : pane.reconcile("query");
        const first = invoke(firstMode);
        await waiting;
        await fixture.tick();
        const busyWhileSaving = pane.submitting && pane.send.disabled;
        const second = invoke(secondMode);
        release();
        const settled = await Promise.allSettled([first, second]);
        editor.flush = flush;
        results.push({
          firstMode,
          secondMode,
          busyWhileSaving,
          preparations,
          dispatches,
          errors: settled
            .filter((value) => value.status === "rejected")
            .map((value) => value.reason.message),
          failure: editor.failure?.message ?? null,
          pending: editor.record.pending,
          unlocked: !pane.submitting,
        });
      }
      return results;
    } finally {
      fixture.setReply("prepare_submission");
      fixture.setReply("dispatch_submission");
    }
  });
  for (const result of races)
    assert.deepEqual(result, {
      firstMode: result.firstMode,
      secondMode: result.secondMode,
      busyWhileSaving: true,
      preparations:
        result.firstMode === "submit" && result.secondMode === "submit" ? 1 : 0,
      dispatches: 1,
      errors: [],
      failure: null,
      pending: null,
      unlocked: true,
    });
  const recovery = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      originalReconcile = pane.reconcile;
    const { DraftEditor } = fixture;
    const originalFlush = DraftEditor.prototype.flush;
    let release,
      entered,
      automatic,
      dispatches = 0;
    const held = new Promise((resolve) => {
      release = resolve;
    });
    const waiting = new Promise((resolve) => {
      entered = resolve;
    });
    try {
      const store = fixture.runtime.connection.drafts,
        session = "automatic-reconcile-overlap";
      let record = await store.ensure("main", session, "c".repeat(64));
      record = await store.freeze(record, {
        kind: "message",
        id: session,
        opaque: "{}",
        text_bytes: 0,
        images: 0,
        references: 0,
      });
      await store.begin(record);
      DraftEditor.prototype.flush = async function () {
        entered();
        await held;
        return originalFlush.call(this);
      };
      pane.reconcile = function (mode) {
        const result = originalReconcile.call(this, mode);
        automatic ??= result;
        return result;
      };
      fixture.setReply("dispatch_submission", async () => {
        dispatches++;
        return JSON.stringify({ status: "complete", receipt: "{}" });
      });
      await fixture.surface(session);
      await pane.binding;
      await waiting;
      const manual = pane.reconcile("query");
      release();
      await Promise.all([automatic, manual]);
      DraftEditor.prototype.flush = originalFlush;
      const failure = pane.editor.failure?.message ?? null,
        pending = pane.editor.record.pending;
      const failures = [];
      pane.editor.flush = async () => {
        throw new Error("injected save failure");
      };
      for (const mode of ["submit", "query"]) {
        let error;
        try {
          await (mode === "submit"
            ? pane.submit("primary")
            : pane.reconcile("query"));
        } catch (reason) {
          error = reason.message;
        }
        failures.push({ error, unlocked: !pane.submitting });
      }
      delete pane.editor.flush;
      return { dispatches, failure, pending, failures };
    } finally {
      release();
      fixture.setReply("dispatch_submission");
      pane.reconcile = originalReconcile;
      DraftEditor.prototype.flush = originalFlush;
    }
  });
  assert.deepEqual(recovery, {
    dispatches: 1,
    failure: null,
    pending: null,
    failures: [
      { error: "injected save failure", unlocked: true },
      { error: "injected save failure", unlocked: true },
    ],
  });
  const accounting = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts;
    const sibling = new fixture.DraftEditor(store, {
      ...store.blank("main", "inactive-editor", "c".repeat(64)),
      text: "界".repeat(4096),
    });
    pane.editors.set("inactive-editor", sibling);
    const encode = TextEncoder.prototype.encode;
    let inactiveEncodes = 0;
    try {
      TextEncoder.prototype.encode = function (value) {
        if (value === sibling.text) inactiveEncodes++;
        return encode.call(this, value);
      };
      pane.edit("active UTF-8 🦀");
    } finally {
      TextEncoder.prototype.encode = encode;
    }
    await pane.flush();
    // Imported/restored editor text uses the same accounting as a keystroke.
    sibling.text = "界".repeat(349525); // 1 MiB minus one byte.
    const second = new fixture.DraftEditor(store, {
      ...store.blank("main", "full-editor", "c".repeat(64)),
      text: "x".repeat(1024 * 1024),
    });
    pane.editors.set("full-editor", second);
    const previous = pane.editor.text;
    let bounded = false;
    try {
      pane.edit("🦀");
    } catch (error) {
      bounded = error.message.includes("2 MiB");
    }
    pane.editors.delete("inactive-editor");
    pane.editors.delete("full-editor");
    return {
      inactiveEncodes,
      bounded,
      retained: pane.editor.text === previous,
    };
  });
  assert.deepEqual(accounting, {
    inactiveEncodes: 0,
    bounded: true,
    retained: true,
  });
  const saveGeometry = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts,
      edit = store.edit;
    let release, entered;
    const held = new Promise((resolve) => {
      release = resolve;
    });
    const started = new Promise((resolve) => {
      entered = resolve;
    });
    store.edit = async function (...args) {
      entered();
      await held;
      return edit.apply(this, args);
    };
    try {
      pane.edit("Save without moving pointer targets");
      await started;
      await fixture.tick();
      const saving = pane.node
        .querySelector(".draft-status")
        .getBoundingClientRect().height;
      const savingText = pane.node.querySelector(".draft-status").textContent;
      const measure = () => Object.fromEntries(
        [".composer", ".composer textarea", "[data-testid=composer-send]"].map(selector => {
          const {x, y, width, height} = pane.node.querySelector(selector).getBoundingClientRect();
          return [selector, {x, y, width, height}];
        }),
      );
      const whileSaving = measure();
      const completion = pane.editor.saving;
      release();
      await completion;
      await fixture.tick();
      return {
        saving,
        savingText,
        whileSaving,
        afterSave: measure(),
        saved: pane.node.querySelector(".draft-status").getBoundingClientRect()
          .height,
      };
    } finally {
      release();
      store.edit = edit;
    }
  });
  assert(
    saveGeometry.saving === 0 && saveGeometry.saved === 0 &&
      saveGeometry.savingText === "",
    JSON.stringify(saveGeometry),
  );
  assert.deepEqual(saveGeometry.whileSaving, saveGeometry.afterSave,
    "Autosave must not move the composer, input or Send button");
  await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"), store = fixture.runtime.connection.drafts,
      edit = store.edit;
    store.edit = async () => { throw new Error("Autosave fixture failure"); };
    try {
      pane.edit("Keep input after an autosave failure");
      await pane.editor.saving;
      await fixture.tick();
    } finally { store.edit = edit; }
  });
  await page.getByText("Input is not saved. Autosave fixture failure", {exact: true}).waitFor();
  assert(await page.getByRole("button", {name: "Use saved input", exact: true}).isVisible());
  await page.getByRole("button", {name: "Replace saved text and images", exact: true}).click();
  await page.waitForFunction(() => !fixture.runtime.panes.get("main").editor.failure);
  assert.equal(await restored.inputValue(), "Keep input after an autosave failure");
  const close = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts;
    const inactive = new fixture.DraftEditor(
      store,
      await store.ensure("main", "failed-inactive", "c".repeat(64)),
    );
    inactive.text = "unsaved inactive input";
    inactive.dirty = true;
    inactive.failure = new Error("injected inactive save failure");
    const key = JSON.stringify(inactive.record.key);
    pane.editors.set(key, inactive);
    const active = pane.editor;
    await pane.flush();
    let error;
    try {
      await pane.flush(true);
    } catch (failure) {
      error = failure.message;
    }
    const preserved =
      pane.editor === active &&
      pane.editors.get(key) === inactive &&
      inactive.text === "unsaved inactive input";
    await inactive.resolve(false);
    await pane.flush(true);
    return {
      error,
      preserved,
      saved: (await store.get("main", "failed-inactive")).text,
    };
  });
  assert.deepEqual(close, {
    error: "injected inactive save failure",
    preserved: true,
    saved: "unsaved inactive input",
  });
  return {
    header_before_draft: switching,
    restored_draft: true,
    overlapping_flows: races.length,
    automatic_reconciliation: true,
    failed_save_releases_admission: true,
    cached_utf8_accounting: true,
    inactive_close_recovery: true,
    visible_save_failure_recovery: true,
    save_geometry: saveGeometry,
  };
}
