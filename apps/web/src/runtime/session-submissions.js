import {boundedImage} from '../../image-bounds.js';

export async function exportSession(pane, context, args) {
  if (pane.exportAbort) {
    pane.exportAbort.abort();
    return;
  }
  if (!pane.generation)
    throw new Error("Open a native conversation first");
  const owner = context.connection;
  if (!owner) throw new Error("Not connected");
  const abort = new AbortController(),
    generation = pane.generation;
  pane.exportAbort = abort;
  pane.renderComposer();
  const closed = () => abort.abort();
  window.addEventListener("pagehide", closed, { once: true });
  try {
    await context.downloadSession(
      context.boundCall(owner),
      pane.index,
      generation,
      args,
      abort.signal,
    );
  } finally {
    window.removeEventListener("pagehide", closed);
    pane.exportAbort = undefined;
    pane.renderComposer();
  }
}

export async function submit(pane, context, choice = "primary") {
  if (!pane.generation) return pane.startConversation();
  // Capture before flushing storage: a later frame cannot reinterpret this click.
  const displayed = pane.composerActions;
  const action = displayed?.[choice];
  if (pane.submitting || pane.uploading) return;
  if (pane.editor && /^\s*\/export(?:\s|$)/.test(pane.editor.text)) {
    if (pane.editor.images.length || pane.editor.references.length)
      throw new Error("Export does not accept attachments");
    const text = pane.editor.text;
    await pane.exportSession(text.replace(/^\s*\/export/, ""));
    return;
  }
  const pending = pane.editor?.record.pending;
  pane.submitting = pending?.kind === "command" && pending.phase !== "prepared"
    ? "Checking command result" : "Sending message";
  pane.renderComposer();
  try {
    await pane.flush();
    if (!pane.editor || pane.bindingError)
      throw new Error(pane.bindingError ?? "Open a conversation first");
    const editor = pane.editor,
      generation = pane.generation,
      current = context.connection;
    let mode = "dispatch";
    if (!editor.record.pending) {
      if (!action)
        throw new Error(
          "Wait for the sending options to refresh. Your draft is retained.",
        );
      const captured = editor.savedInput;
      const references = pane.referenceState(editor);
      references.preparing = captured.references.map(reference => ({target: reference.metadata.target, snapshot: reference.snapshot}));
      try {
        const prepared = JSON.parse(
          await context.call(
            "prepare_submission",
            JSON.stringify({
              pane: pane.index,
              generation,
              text: captured.text,
              images: captured.images,
              references: captured.references,
              sandbox: action.id === 'steer' ? null : pane.composerView.access ?? null,
              action_revision: displayed.revision,
              action: action.id,
            }),
          ),
        );
        if (
          context.connection !== current ||
          current.closing ||
          pane.editor !== editor
        )
          throw new Error(
            "Conversation changed before request preparation completed",
          );
        await editor.update(() => editor.store.startDispatch(captured, prepared));
      } finally {
        references.preparing = undefined;
        void pane.drainReferenceReleases(editor, generation).catch(() => {});
      }
    } else if (editor.record.pending.phase === "prepared") {
      await editor.update(record => editor.store.claimPrepared(record));
    } else {
      mode = editor.record.pending.kind === "command" ? "query" : "retry_message";
    }
    await pane.executePending(editor, generation, current, mode);
  } finally {
    pane.submitting = undefined;
    pane.renderComposer();
  }
}

export async function reconcile(pane, context, mode) {
  if (pane.submitting || !pane.editor || pane.bindingError) return;
  const editor = pane.editor,
    generation = pane.generation,
    current = context.connection;
  pane.submitting = mode === "query" ? "Checking previous result" : "Sending saved request";
  pane.renderComposer();
  try {
    await editor.flush();
    if (mode === "dispatch")
      await editor.update(record => editor.store.claimPrepared(record));
    await pane.executePending(editor, generation, current, mode);
  } finally {
    pane.submitting = undefined;
    pane.renderComposer();
  }
}

export async function executePending(
  pane,
  context,
  editor,
  generation,
  current,
  mode,
) {
  if (!editor.record.pending) return;
  const expected = editor.record;
  let result;
  if (
    context.connection !== current ||
    current.closing ||
    pane.editor !== editor
  ) {
    result = {
      status: mode === "dispatch" ? "not_admitted" : "unknown",
      error: "Connection changed before dispatch",
    };
  } else {
    try {
      result = JSON.parse(
        await context.call("dispatch_submission", {
          pane: pane.index,
          generation,
          opaque: expected.pending.opaque,
          mode,
        }),
      );
    } catch (error) {
      result = {
        status:
          error.notAdmitted && mode === "dispatch" ? "not_admitted" : "unknown",
        error: error.message,
      };
    }
  }
  await editor.settle(expected, result);
  if (result.status === "unknown" && expected.pending.kind === "message" && pane.editor === editor) {
    editor.deliveryDetail = {id: expected.pending.id, error: result.error};
    return;
  }
  if (editor.deliveryDetail?.id === expected.pending.id) editor.deliveryDetail = undefined;
  if (result.status === "complete" && pane.editor === editor && !editor.text)
    pane.closeCompletions();
  if (result.notice) context.notify(result.notice);
  if (result.status === "rejected" && expected.pending.kind === "command") {
    context.setCommandWarning({id: expected.pending.id, text: result.error, anchor: pane.node});
    return;
  }
  if (result.status !== "complete")
    throw new Error(
      result.error ?? "The original submission still needs confirmation",
    );
}

export async function submitQueue(pane, context, queue, generation) {
  const editor = pane.editor,
    current = context.connection;
  if (!editor || pane.submitting || editor.record.pending || pane.bindingError)
    throw new Error(
      "Resolve the previous saved request before editing the queue",
    );
  pane.submitting = "Updating queue";
  pane.renderComposer();
  try {
    await editor.flush();
    const prepared = JSON.parse(
      await context.call(
        "prepare_submission",
        JSON.stringify({ pane: pane.index, generation, queue }),
      ),
    );
    if (
      pane.editor !== editor ||
      pane.generation !== generation ||
      context.connection !== current ||
      current.closing
    )
      throw new Error("Conversation changed before saving the queue request");
    await editor.update((record) => editor.store.startDispatch(record, prepared));
    await pane.executePending(editor, generation, current, "dispatch");
  } finally {
    pane.submitting = undefined;
    pane.renderComposer();
  }
}

export async function upload(pane, context, files) {
  if (!files.length) return;
  if (pane.uploading || !pane.generation || !pane.editor)
    throw new Error("Image import is unavailable while this pane is busy");
  const limits = context.view.media_limits,
    editor = pane.editor,
    generation = pane.generation,
    current = context.connection;
  if (editor.images.length + files.length > limits.images)
    throw new Error(`A draft can hold at most ${limits.images} images`);
  if (files.some((file) => !file.size || file.size > limits.upload_bytes))
    throw new Error("Each image source must contain 1 byte to 16 MiB");
  const batch = {editor, files, next:0};
  pane.uploading = batch;
  pane.renderComposer();
  try {
    for (const file of files) {
      const bytes = await file.arrayBuffer();
      if (
        context.connection !== current ||
        current.closing ||
        pane.generation !== generation
      )
        throw new Error("Pane changed; remaining images were not imported");
      // Only reuse raster bytes that the document can safely hand to its decoder.
      // Other imports use Media's canonical thumbnail, including animated originals.
      const source = new Uint8Array(bytes);
      let previewable = false;
      try { previewable = boundedImage(source, file.type).bytes === source; } catch { /* Canonical fallback. */ }
      const media = JSON.parse(
        await context.call(
          "import_image",
          { pane: pane.index, generation, bytes },
          [bytes],
        ),
      );
      if (previewable && context.connection === current && !current.closing && pane.editor === editor) {
        context.previews(current).rememberDraft(media, file);
      }
      batch.next++;
      editor.edit(editor.text, [...editor.images, media]);
      await editor.flush();
    }
  } finally {
    pane.uploading = undefined;
    pane.renderComposer();
  }
}

export async function executeDocumentCommand(pane, context, name, input = '', consume, revision) {
  if (!/^[a-zA-Z0-9_.-]{1,128}$/.test(name) || typeof input !== 'string' || new TextEncoder().encode(input).length > 16 * 1024)
    throw new Error('Invalid Session command');
  const editor = pane.editor, generation = pane.generation, current = context.connection;
  if (!editor || !generation || !current || current.closing || pane.submitting || pane.uploading || pane.switching || editor.record.pending || pane.bindingError)
    throw new Error('Resolve the pending operation before using a Session command');
  pane.submitting = "Running command";
  pane.renderComposer();
  try {
    await editor.flush();
    if (revision !== undefined && revision !== editor.revision) throw new Error('The input changed; select the command again');
    const captured = editor.savedInput;
    const prepared = JSON.parse(await context.call('prepare_submission', JSON.stringify({
      pane: pane.index, generation, text: `/${name} ${input}`, images: [], references: [],
    })));
    if (prepared.kind !== 'command') throw new Error('Session command is unavailable');
    if (pane.editor !== editor || pane.generation !== generation || context.connection !== current || current.closing)
      throw new Error('Conversation changed before command preparation completed');
    // Keep the pre-preparation editRevision: typing during preparation must prevent consuming that captured span.
    await editor.update(() => editor.store.startDispatch(captured, prepared, true, consume));
    await pane.executePending(editor, generation, current, 'dispatch');
  } finally {
    pane.submitting = undefined;
    pane.renderComposer();
  }
}
