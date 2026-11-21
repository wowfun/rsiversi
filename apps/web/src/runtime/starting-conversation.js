import {StartingDraft} from '../../starting-draft.js';

export async function bindStarting(pane, context, binding) {
  const connection = context.connection;
  if (!connection?.drafts || connection.closing) return;
  const draft = await StartingDraft.load(connection.drafts.namespace, pane.index);
  if (context.connection !== connection || connection.closing || pane.startBindingOwner !== binding) return;
  pane.starting = draft;
  pane.startingStorageError = undefined;
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
export async function restoreStartingDraft(pane, context, row) {
  if (pane.submitting || pane.switching) throw new Error('Wait for the current conversation operation');
  const connection = context.connection, recovery = pane.recoveryView;
  const key = JSON.parse(row.key), namespace = JSON.stringify(key.slice(0,2));
  const current = () => {
    if (context.connection !== connection || connection?.closing || pane.recoveryView !== recovery ||
        JSON.stringify(connection?.drafts?.namespace) !== namespace)
      throw new Error('Connection or recovery panel changed; the starting draft is saved');
  };
  current();
  await pane.flush();
  current();
  const draft = await StartingDraft.load(key.slice(0,2),key[2]);
  current();
  if (draft.record.incarnation !== row.incarnation || draft.record.revision !== row.revision)
    throw new Error('This draft changed in another window; reopen Saved drafts');
  return openComposer(pane, context, draft, current);
}
async function openComposer(pane, context, draft = pane.starting, recoveryCurrent = () => {}) {
  const connection = context.connection;
  const current = () => {
    recoveryCurrent();
    if (context.connection !== connection || connection?.closing)
      throw new Error('Connection changed; your starting draft is saved');
  };
  current();
  if (!draft) return false;
  if (!context.view?.setup?.agent?.default_model) { context.openModels(); return false; }
  const workspace = draft.workspace
    ? context.view?.catalog.workspaces.find(entry => entry.id === draft.workspace)
    : context.view?.catalog.workspaces[0];
  if (!workspace) { window.dispatchEvent(new Event('rsi:choose-workspace')); return false; }
  pane.submitting = "Opening conversation"; pane.renderComposer();
  try {
    await draft.flush();
    current();
    const text = draft.text, serial = draft.serial, generation = pane.generation;
    const selection = draft === pane.starting && pane.input?.value === text
      ? [pane.input.selectionStart,pane.input.selectionEnd] : undefined;
    const preset = draft === pane.starting ? pane.stagedPreset : undefined;
    await context.boundCall(connection)('command', JSON.stringify({action:'create', pane:pane.index, workspace:workspace.id, agent_preset_id:preset}));
    // A command response may precede its React frame/attachment commit.
    await new Promise((resolve, reject) => {
      let unsubscribe;
      const timer = setTimeout(() => { unsubscribe?.(); reject(new Error('Conversation is still opening. Your starting draft is saved.')); }, 15000);
      const check = () => {
        try { current(); }
        catch(error) { clearTimeout(timer); unsubscribe?.(); reject(error); return; }
        if (pane.generation && pane.generation !== generation && pane.editor && pane.staged?.data?.creation?.workspace_id === workspace.id && (!preset || pane.staged.data.agent_preset === preset)) { clearTimeout(timer); unsubscribe?.(); resolve(); }
      };
      unsubscribe = pane.subscribe(check); check();
    });
    current();
    const editor = pane.editor;
    if (editor.text || editor.images.length || editor.references.length || editor.record.pending)
      throw new Error('Conversation already has input. Your starting draft is saved.');
    if (text) { editor.edit(text, [], []); await editor.flush(); }
    current();
    if (pane.editor !== editor) throw new Error('Conversation changed; your starting draft is saved');
    if (selection && pane.editor === editor && pane.input.value === text && draft.serial === serial)
      pane.input.setSelectionRange(...selection);
    // Never clear a newer starting edit or send through a replacement attachment.
    await draft.clearCaptured(serial);
    if (context.connection !== connection || connection.closing || pane.editor !== editor)
      throw new Error('Conversation changed. Input is saved in its original conversation.');
  } finally { pane.submitting = undefined; pane.renderComposer(); }
  return true;
}
export async function startConversation(pane, context) {
  if (!pane.starting?.text.trim()) return;
  if (!await prepareComposer(pane, context)) return;
  return pane.submit('primary');
}
