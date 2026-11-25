import {StartingDraft} from '../../starting-draft.js';
import {restoreStartingDraft} from './starting-conversation.js';
import {conversationTitle, navigationTitle} from '../../conversation-title.js';
const preview = text => Array.from(text.slice(0,160)).slice(0,80).join('').replace(/\s+/g,' ').trim();

export async function restoreDrafts(pane, context) {
  const current = context.connection,
    store = current?.drafts;
  if (!store) throw new Error("Connect to your service first");
  const panel = {
    rows: [],
    busy: true,
    loading: true,
    discard: async () => {
      if (!active() || panel.busy) return;
      panel.close();
      await pane.discardSavedDrafts();
    },
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
  pane.recoveryView = panel;
  pane.invalidate();
  const appendSession = record => {
    const local = pane.editors.get(JSON.stringify(record.key));
    if (local && local === pane.editor && !local.dirty && !local.failure) return;
    const surface = Object.keys(context.view?.surfaces ?? {}).find(key=>context.view.surfaces[key]?.kind==='native' && context.view.surfaces[key].session===record.key[3]);
    const row = {
      id: JSON.stringify(record.key),
      coordinates: JSON.stringify(record.key),
      text: record.text,
      removable: !record.pending && !record.references.length && !record.releases.length && !local?.dirty && !local?.failure,
      cancelDelete: () => { if (active() && !panel.busy) { row.deleteConfirmed=false; pane.invalidate(); } },
      summary: [surface ? conversationTitle(context.view,surface) : navigationTitle(context.view,record.key[3]) || record.key[3], record.pending ? "awaiting confirmation" :
        preview(record.text),
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
        if (local?.dirty || local?.failure || (local && local === pane.editor)) throw new Error("Save or resolve this editor before deleting its draft");
        if (!row.empty && !row.removable) return;
        if (!row.empty && !row.deleteConfirmed) { row.deleteConfirmed=true; pane.invalidate(); return; }
        panel.busy = true;
        pane.invalidate();
        try {
          if (row.empty) await store.removeEmpty(record);
          else await store.removeSaved(record);
          pane.editors.delete(JSON.stringify(record.key));
          if (active()) remove(row);
        } finally {
          panel.busy = false;
          pane.invalidate();
        }
      },
    };
    panel.rows.push(row);
  };
  let sessionFailure;
  const starts = [undefined];
  const loadSessions = async (index, after) => {
    panel.loading = panel.busy = true; pane.invalidate();
    try {
      let page;
      try {
        page = await store.list(pane.index,after);
        sessionFailure = undefined;
      } catch (error) {
        sessionFailure = `Saved drafts could not be read: ${error.message}`;
        if (panel.rows.length || index) return;
        page = {rows:[]};
      }
      if (!active()) return;
      starts[index] = after;
      const startingRows = panel.rows.filter(row => row.starting);
      panel.rows = [];
      const records = new Map(page.rows.map(record => [JSON.stringify(record.key),record]));
      if (!index) for (const [key, local] of pane.editors) {
        if (local.record.key[2] === pane.index &&
            JSON.stringify(local.record.key.slice(0, 2)) === JSON.stringify(store.namespace) &&
            (local.dirty || local.failure) && !records.has(key)) records.set(key,local.record);
      }
      for (const record of records.values()) appendSession(record);
      panel.rows.push(...startingRows);
      const navigate = (target, cursor) => () => { if (active() && !panel.busy) return loadSessions(target,cursor); };
      panel.previousSessions = index ? navigate(index-1,starts[index-1]) : undefined;
      panel.nextSessions = page.next ? navigate(index+1,page.next) : undefined;
    } finally {
      if (active()) {
        panel.notice = [sessionFailure,pane.startingStorageError].filter(Boolean).join(' ');
        panel.loading = panel.busy = false; pane.invalidate();
      }
    }
  };
  await loadSessions(0);
  if (!active()) return;
  let next;
  const loadStarting = async () => {
    panel.loading = panel.busy = true; panel.notice = sessionFailure; pane.invalidate();
    try {
      const page = await StartingDraft.list(store.namespace,next);
      if (!active()) return;
      next = page.next;
      for (const saved of page.rows) {
        if (!pane.generation && pane.starting?.key === saved.key && !pane.starting.dirty && !pane.starting.failure) continue;
        const surface = JSON.parse(saved.key)[2];
        const row = {
          id: saved.key, coordinates:saved.key, starting:true, text:saved.text, empty:false,
          summary:`Starting draft · ${surface==='main'?'Main':surface==='compare'?'Compare':surface} · ${preview(saved.text) || 'Workspace selection'}`,
          cancelDelete: () => { if (active() && !panel.busy) { row.deleteConfirmed=false; pane.invalidate(); } },
          open: async () => {
            if (!active() || panel.busy) return;
            panel.busy=true; pane.invalidate();
            try { if (await restoreStartingDraft(pane,context,saved)) panel.close(); }
            finally { panel.busy=false; pane.invalidate(); }
          },
          remove: async () => {
            if (!active() || panel.busy) return;
            if (!row.deleteConfirmed) { row.deleteConfirmed=true; pane.invalidate(); return; }
            panel.busy=true; pane.invalidate();
            try { await StartingDraft.remove(saved); if (active()) remove(row); }
            finally { panel.busy=false; pane.invalidate(); }
          },
        };
        panel.rows.push(row);
      }
      panel.more = next ? async () => {
        if (!active() || panel.busy) return;
        panel.busy=true; pane.invalidate();
        await loadStarting();
      } : undefined;
    } catch (error) {
      if (active()) {
        pane.startingStorageError = `Starting drafts could not be read: ${error.message}`;
        panel.notice = [sessionFailure, pane.startingStorageError].filter(Boolean).join(' ');
        pane.renderComposer();
      }
    } finally { panel.loading = panel.busy = false; pane.invalidate(); }
  };
  await loadStarting();
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
          pane.editor.adoptMoved(moved);
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
