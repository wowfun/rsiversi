export async function restoreDrafts(pane, context) {
  const current = context.connection,
    store = current?.drafts;
  if (!store) throw new Error("Connect to your service first");
  const records = new Map((await store.list(pane.index)).map(record => [JSON.stringify(record.key), record]));
  if (context.connection !== current || current.closing) return;
  for (const [key, local] of pane.editors) {
    if (local.record.key[2] === pane.index &&
        JSON.stringify(local.record.key.slice(0, 2)) === JSON.stringify(store.namespace) &&
        (local.dirty || local.failure) && !records.has(key)) records.set(key, local.record);
  }
  const panel = {
    rows: [],
    busy: false,
    close: () => {
      if (pane.recoveryView !== panel) return;
      pane.recoveryView = undefined;
      pane.invalidate();
      requestAnimationFrame(() => {
        if (context.connection === current && !pane.recoveryView) pane.focusInput();
      });
    },
  };
  const active = () =>
    context.connection === current &&
    !current.closing &&
    pane.recoveryView === panel;
  const remove = (row) => {
    panel.rows = panel.rows.filter((value) => value !== row);
    pane.invalidate();
  };
  for (const record of records.values()) {
    const local = pane.editors.get(JSON.stringify(record.key));
    const row = {
      id: JSON.stringify(record.key),
      text: local?.text ?? record.text,
      summary: [record.key[3], record.pending ? "awaiting confirmation" :
        Array.from(record.text.slice(0, 160)).slice(0, 80).join("").replace(/\s+/g, " ").trim(),
        record.images.length ? `${record.images.length} images` : "",
        record.releases.length ? "pending reference cleanup" : ""].filter(Boolean).join(" · "),
      empty:
        !record.text &&
        !record.images.length &&
        !record.references.length &&
        !record.releases.length &&
        !record.pending,
      unsaved:
        local?.dirty || local?.failure
          ? {
              text: local.text,
              message:
                local.failure?.message ??
                "Resolve this tab's input before recovering the conversation.",
              resolve: async (useSaved) => {
                await local.resolve(useSaved);
                if (active()) await pane.restoreDrafts();
              },
            }
          : undefined,
      open: async () => {
        if (!active() || panel.busy) return;
        panel.busy = true;
        pane.invalidate();
        try {
          await pane.flush().catch(() => {});
          if (!active()) return;
          pane.switching = true;
          pane.renderComposer();
          const result = JSON.parse(
            await context.call(
              "restore_session",
              JSON.stringify({
                action: "open",
                pane: pane.index,
                session: record.key[3],
                header: record.header,
              }),
            ),
          );
          if (!active()) return;
          if (result.status === "expired") {
            row.expired = true;
            row.canRecreate =
              !!record.creation && !record.everDispatched && !record.pending &&
              !record.references.length && !record.releases.length;
            if (record.references.length && !record.pending) row.retireReferences = async () => {
              if (!active() || panel.busy) return;
              panel.busy = true; pane.invalidate();
              try {
                const editor = local ?? new context.DraftEditor(store, record);
                await editor.flush();
                if (!active()) return;
                if (editor.record.pending) throw new Error("Resolve the original submission before removing its references");
                editor.edit(editor.text, editor.images, []);
                await editor.flush();
                if (!active()) return;
                await pane.drainReferenceReleases(editor, pane.generation, true);
                if (active()) await pane.restoreDrafts();
              } finally { panel.busy = false; pane.invalidate(); }
            };
          } else panel.close();
        } finally {
          panel.busy = false;
          if (context.connection === current) {
            pane.switching = false;
            pane.renderComposer();
          } else pane.invalidate();
        }
      },
      cleanup: record.releases.length ? async () => {
        if (!active() || panel.busy) return;
        panel.busy = true;
        pane.invalidate();
        try {
          const editor = local ?? new context.DraftEditor(store, record);
          await editor.flush();
          if (!active()) return;
          await pane.drainReferenceReleases(editor, pane.generation, true);
          if (active()) await pane.restoreDrafts();
        } finally {
          panel.busy = false;
          pane.invalidate();
        }
      } : undefined,
      recreate: async () => {
        if (!active() || panel.busy) return;
        panel.busy = true;
        pane.invalidate();
        try {
          await pane.recreateDraft(record, current);
          if (active()) remove(row);
        } finally {
          panel.busy = false;
          pane.invalidate();
        }
      },
      remove: async () => {
        if (!active() || panel.busy) return;
        panel.busy = true;
        pane.invalidate();
        try {
          await store.removeEmpty(record);
          if (active()) remove(row);
        } finally {
          panel.busy = false;
          pane.invalidate();
        }
      },
    };
    panel.rows.push(row);
  }
  pane.recoveryView = panel;
  pane.invalidate();
}

export async function recreateDraft(pane, context, record, current) {
  if (context.connection !== current || current.closing)
    throw new Error("Connection changed; the draft was left untouched");
  if (pane.recreating || pane.submitting || pane.uploading || pane.switching)
    throw new Error(
      "Wait for this pane's current operation before recovering the draft",
    );
  const store = current.drafts,
    key = JSON.stringify(record.key);
  const source =
    pane.editors.get(key) ?? new context.DraftEditor(store, record);
  pane.recreating = true;
  pane.switching = true;
  pane.renderComposer();
  try {
    await source.transfer(async (expected) => {
      if (source.record.incarnation !== record.incarnation)
        throw new Error(
          "The saved draft was replaced; reopen the recovery list",
        );
      if (!expected.creation || expected.everDispatched || expected.pending)
        throw new Error(
          "This draft is no longer eligible for a new conversation",
        );
      if (context.connection !== current || current.closing)
        throw new Error("Connection changed; the draft was left untouched");
      const fresh = JSON.parse(
        await context.call(
          "restore_session",
          JSON.stringify({
            action: "create",
            pane: pane.index,
            creation: expected.creation,
          }),
        ),
      );
      const blank = await store.ensure(
        pane.index,
        fresh.session,
        fresh.header,
        fresh.creation,
      );
      const moved = await store.moveFresh(expected, blank);
      await pane.binding;
      if (
        context.connection === current &&
        pane.editor &&
        JSON.stringify(pane.editor.record.key) === JSON.stringify(moved.key)
      ) {
        if (pane.editor.dirty || pane.editor.failure) {
          pane.editor.failure = new Error(
            "Saved input was restored while you were typing; choose which editor to keep",
          );
        } else {
          pane.editor.record = moved;
          pane.editor.text = moved.text;
          pane.editor.images = structuredClone(moved.images);
          pane.editor.references = structuredClone(moved.references);
        }
      }
      if (pane.editor === source) pane.editor = undefined;
      pane.editors.delete(key);
    });
  } finally {
    pane.recreating = false;
    pane.switching = false;
    pane.renderComposer();
  }
}
