export async function exportSession(pane, context, args) {
  if (pane.exportAbort) {
    pane.exportAbort.abort();
    return;
  }
  if (!pane.generation || !pane.editor)
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
  pane.submitting = true;
  pane.renderComposer();
  try {
    await pane.flush();
    if (!pane.editor || pane.bindingError)
      throw new Error(pane.bindingError ?? "Open a conversation first");
    const editor = pane.editor,
      generation = pane.generation,
      current = context.connection;
    if (!editor.record.pending) {
      if (!action)
        throw new Error(
          "Wait for the sending options to refresh. Your draft is retained.",
        );
      const captured = editor.record;
      const prepared = JSON.parse(
        await context.call(
          "prepare_submission",
          JSON.stringify({
            pane: pane.index,
            generation,
            text: editor.text,
            images: editor.images,
            references: editor.references,
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
      await editor.update(() => editor.store.freeze(captured, prepared));
    }
    const mode =
      editor.record.pending.phase === "prepared"
        ? "dispatch"
        : editor.record.pending.kind !== "command"
          ? "retry_message"
          : "query";
    await pane.executePending(editor, generation, current, mode);
  } finally {
    pane.submitting = false;
    pane.renderComposer();
  }
}

export async function reconcile(pane, context, mode) {
  if (pane.submitting || !pane.editor || pane.bindingError) return;
  const editor = pane.editor,
    generation = pane.generation,
    current = context.connection;
  pane.submitting = true;
  pane.renderComposer();
  try {
    await editor.flush();
    await pane.executePending(editor, generation, current, mode);
  } finally {
    pane.submitting = false;
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
  if (mode === "dispatch")
    await editor.update((record) => editor.store.begin(record));
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
  await editor.update(() => editor.store.settle(expected, result));
  if (result.notice) context.notify(result.notice);
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
  pane.submitting = true;
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
    await editor.update((record) => editor.store.freeze(record, prepared));
    await pane.executePending(editor, generation, current, "dispatch");
  } finally {
    pane.submitting = false;
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
  pane.uploading = true;
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
      const media = JSON.parse(
        await context.call(
          "import_image",
          { pane: pane.index, generation, bytes },
          [bytes],
        ),
      );
      editor.edit(editor.text, [...editor.images, media]);
      await editor.flush();
    }
  } finally {
    pane.uploading = false;
    pane.renderComposer();
  }
}
