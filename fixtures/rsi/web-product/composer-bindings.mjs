import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
export async function verifyComposerBindings(page, report, name) {
  const draftEcho = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main");
    const data = {
      generation: "draft-echo",
      session: "retained-session",
      path: "/workspace",
      model: { deployment: "test", model: "model" },
      transcript: { blocks: [], status: "Ready", omitted: false },
      pending: [],
      notice: "",
    };
    let submitted;
    try {
      fixture.setReply("prepare_submission", async (payload) => {
        submitted = JSON.parse(payload).text;
        return JSON.stringify({
          kind: "message",
          id: "message-one",
          opaque: payload,
          text_bytes: submitted.length,
          images: 0,
          references: 0,
        });
      });
      fixture.setReply("dispatch_submission", async () =>
        JSON.stringify({ status: "complete", receipt: "{}" }),
      );
      await fixture.surface(data.session, data);
      await pane.binding;
      pane.input.focus();
      pane.input.value = "locally persisted draft";
      pane.input.dispatchEvent(new Event("input", { bubbles: true }));
      await pane.flush();
      pane.send.focus();
      await fixture.surface(data.session, {
        ...data,
        draft: "obsolete Worker input",
      });
      const retained = pane.input.value;
      await pane.submit("primary");
      await fixture.surface(data.session, {
        ...data,
        draft: "locally persisted draft",
      });
      return { retained, submitted, cleared: pane.input.value };
    } finally {
      fixture.setReply("prepare_submission");
      fixture.setReply("dispatch_submission");
    }
  });
  assert.deepEqual(draftEcho, {
    retained: "locally persisted draft",
    submitted: "locally persisted draft",
    cleared: "",
  });
  const pendingDraftFlush = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts,
      ensure = store.ensure,
      edit = store.edit;
    let release, releaseSave, savingStarted;
    const gate = new Promise((resolve) => {
        release = resolve;
      }),
      saveGate = new Promise((resolve) => {
        releaseSave = resolve;
      });
    const entered = new Promise((resolve) => {
      savingStarted = resolve;
    });
    const data = {
      session: "pending-binding",
      header: "c".repeat(64),
      creation: null,
    };
    try {
      store.ensure = async (...args) => {
        await gate;
        return ensure.apply(store, args);
      };
      store.edit = async (...args) => {
        savingStarted();
        await saveGate;
        return edit.apply(store, args);
      };
      pane.binding = pane
        .bindDraft(data)
        .then(() => pane.edit("saved after binding 界"));
      let settled = false;
      const saving = pane.flush().then(() => {
        settled = true;
      });
      const unbound = pane.editor === undefined;
      release();
      await entered;
      await new Promise((resolve) => setTimeout(resolve, 0));
      const waitedForSave = !settled;
      releaseSave();
      await saving;
      await pane.editor.flush();
      const record = (await store.list(pane.index)).find(
        (record) => record.key[3] === data.session,
      );
      return {
        unbound,
        waitedForSave,
        saved: record?.text,
        dirty: pane.editor.dirty,
      };
    } finally {
      store.ensure = ensure;
      store.edit = edit;
    }
  });
  assert.deepEqual(pendingDraftFlush, {
    unbound: true,
    waitedForSave: true,
    saved: "saved after binding 界",
    dirty: false,
  });
  await writeFile(
    join(report, `${name}-pending-draft-flush.json`),
    JSON.stringify(pendingDraftFlush),
  );
  const capturedDraftFlush = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main"),
      store = fixture.runtime.connection.drafts,
      edit = store.edit;
    let releaseOld, releaseNew, savingOld, savingNew;
    const oldGate = new Promise((resolve) => {
        releaseOld = resolve;
      }),
      newGate = new Promise((resolve) => {
        releaseNew = resolve;
      });
    const oldEntered = new Promise((resolve) => {
        savingOld = resolve;
      }),
      newEntered = new Promise((resolve) => {
        savingNew = resolve;
      });
    try {
      store.edit = async (record, text, ...rest) => {
        if (text === "captured draft") {
          savingOld();
          await oldGate;
        }
        if (text === "later draft") {
          savingNew();
          await newGate;
        }
        return edit.call(store, record, text, ...rest);
      };
      const old = pane.editor;
      pane.edit("captured draft");
      await oldEntered;
      pane.binding = pane
        .bindDraft({
          session: "later-binding",
          header: "c".repeat(64),
          creation: null,
        })
        .then(() => pane.edit("later draft"));
      let settled = false;
      const flush = pane.flush(true).then(() => {
        settled = true;
      });
      await newEntered;
      releaseOld();
      await old.flush();
      await new Promise((resolve) => setTimeout(resolve, 0));
      const result = {
        capturedSaved: !old.dirty,
        completed: settled,
        laterStillSaving: pane.editor.dirty,
      };
      releaseNew();
      await pane.editor.flush();
      await flush;
      return result;
    } finally {
      releaseOld();
      releaseNew();
      store.edit = edit;
    }
  });
  assert.deepEqual(capturedDraftFlush, {
    capturedSaved: true,
    completed: true,
    laterStillSaving: true,
  });
  await writeFile(
    join(report, `${name}-captured-draft-flush.json`),
    JSON.stringify(capturedDraftFlush),
  );
  const retries = await page.evaluate(async () => {
    const pane = fixture.runtime.panes.get("main");
    const results = [];
    try {
      fixture.setReply("dispatch_submission", async () =>
        JSON.stringify({ status: "complete", receipt: "{}" }),
      );
      for (const text of ["original", "edited next draft"]) {
        pane.edit("original");
        await pane.flush();
        const editor = pane.editor;
        await editor.update((record) =>
          editor.store.freeze(record, {
            kind: "message",
            id: `retry-${text.length}`,
            opaque: "{}",
            text_bytes: 8,
            images: 0,
            references: 0,
          }),
        );
        await editor.update((record) => editor.store.begin(record));
        if (text !== "original") {
          pane.edit(text);
          await pane.flush();
        }
        pane.renderComposer();
        await fixture.tick();
        const label = pane.send.textContent,
          steerDisabled = pane.steer.disabled;
        await pane.submit("primary");
        results.push({
          text,
          remaining: pane.input.value,
          label,
          steerDisabled,
        });
      }
      return results;
    } finally {
      fixture.setReply("prepare_submission");
      fixture.setReply("dispatch_submission");
    }
  });
  assert.deepEqual(retries, [
    {
      text: "original",
      remaining: "",
      label: "Retry previous",
      steerDisabled: true,
    },
    {
      text: "edited next draft",
      remaining: "edited next draft",
      label: "Retry previous",
      steerDisabled: true,
    },
  ]);
  const ime = await page.evaluate(() => {
    const pane = fixture.runtime.panes.get("main"),
      original = pane.submit;
    let submitted = 0;
    pane.submit = async () => {
      submitted++;
    };
    try {
      for (const options of [{ isComposing: true }, { keyCode: 229 }])
        pane.input.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "Enter",
            ctrlKey: true,
            bubbles: true,
            cancelable: true,
            ...options,
          }),
        );
      const composing = submitted;
      pane.input.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          ctrlKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
      return { composing, after: submitted };
    } finally {
      pane.submit = original;
    }
  });
  assert.deepEqual(ime, { composing: 0, after: 1 });
  return { draftEcho, pendingDraftFlush, capturedDraftFlush, retries, ime };
}
