import {StartingDraft} from '../../starting-draft.js';

export async function bindStarting(pane, context, binding) {
  const connection = context.connection;
  if (!connection?.drafts || connection.closing) return;
  const draft = await StartingDraft.load(connection.drafts.namespace, pane.index);
  if (context.connection !== connection || connection.closing || pane.startBindingOwner !== binding) return;
  pane.starting = draft;
  draft.changed = () => { if (!pane.generation && pane.starting === draft) pane.renderComposer(); };
  pane.renderComposer();
}
export function startingWorkspace(pane, context) {
  const entries = context.view?.catalog.workspaces ?? [];
  return entries.find(entry => entry.id === pane.starting?.workspace) ?? entries[0];
}
export async function prepareComposer(pane, context) {
  if (pane.openingComposer) return pane.openingComposer;
  if (pane.generation && pane.editor) return true;
  if (pane.submitting || pane.switching) return false;
  pane.openingComposer = openComposer(pane, context);
  try { return await pane.openingComposer; }
  finally { pane.openingComposer = undefined; }
}
async function openComposer(pane, context) {
  const connection = context.connection, draft = pane.starting;
  if (!draft) return false;
  if (!context.view?.setup?.agent?.default_model) { context.openModels(); return false; }
  const workspace = startingWorkspace(pane, context);
  if (!workspace) { window.dispatchEvent(new Event('rsi:choose-workspace')); return false; }
  pane.submitting = true; pane.renderComposer();
  try {
    await draft.flush();
    const text = draft.text, serial = draft.serial;
    await context.boundCall(connection)('command', JSON.stringify({action:'create', pane:pane.index, workspace:workspace.id}));
    // A command response may precede its React frame/attachment commit.
    await new Promise((resolve, reject) => {
      let unsubscribe;
      const timer = setTimeout(() => { unsubscribe?.(); reject(new Error('Conversation is still opening. Your starting draft is saved.')); }, 15000);
      const check = () => {
        if (context.connection !== connection || connection.closing) { clearTimeout(timer); unsubscribe?.(); reject(new Error('Connection changed; your starting draft is saved')); }
        else if (pane.generation && pane.editor && pane.staged?.data?.workspace === workspace.id) { clearTimeout(timer); unsubscribe?.(); resolve(); }
      };
      unsubscribe = pane.subscribe(check); check();
    });
    const editor = pane.editor;
    if (editor.text || editor.images.length || editor.references.length || editor.record.pending)
      throw new Error('Conversation already has input. Your starting draft is saved.');
    editor.edit(text, [], []); await editor.flush();
    // Never clear a newer starting edit or send through a replacement attachment.
    await draft.clearCaptured(serial);
    if (context.connection !== connection || connection.closing || pane.editor !== editor)
      throw new Error('Conversation changed. Input is saved in its original conversation.');
  } finally { pane.submitting = false; pane.renderComposer(); }
  return true;
}
export async function startConversation(pane, context) {
  if (!pane.starting?.text.trim()) return;
  if (!await prepareComposer(pane, context)) return;
  return pane.submit('primary');
}
