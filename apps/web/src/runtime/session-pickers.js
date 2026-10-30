import { createFilePicker } from "./file-picker.js";
import { createHistoryPicker } from "./history-picker.js";
import { createReferencePicker } from "./reference-picker.js";
export function showFilePicker(pane, context) {
  const editor = pane.editor,
    generation = pane.generation,
    current = context.connection;
  if (!editor || !generation || pane.switching)
    throw new Error("Open a conversation first");
  pane.fileDialog?.close();
  pane.closeCompletions();
  const initial = editor.text,
    start = pane.input.selectionStart,
    end = pane.input.selectionEnd;
  const from = start === end && initial[start - 1] === "@" ? start - 1 : start,
    call = context.boundCall(current);
  const picker = createFilePicker({
    active: () =>
      pane.generation === generation &&
      pane.editor === editor &&
      context.connection === current &&
      !current.closing,
    request: async (request) =>
      JSON.parse(
        await call(
          "file_input",
          JSON.stringify({ pane: pane.index, generation, request }),
        ),
      ),
    insert: (locator) => {
      if (editor.text !== initial)
        throw new Error(
          "Draft changed. Close and reopen the picker at the intended position.",
        );
      const text = initial.slice(0, from) + locator + initial.slice(end);
      pane.edit(text, editor.images, editor.references);
      pane.input.value = text;
      pane.input.setSelectionRange(
        from + locator.length,
        from + locator.length,
      );
    },
    changed: () => pane.invalidate(),
    closed: () => {
      if (pane.fileDialog === picker) pane.fileDialog = undefined;
      pane.invalidate();
    },
  });
  picker.id = String(
    (pane.fileDialogSequence = (pane.fileDialogSequence ?? 0) + 1),
  );
  picker.restoreFocus = () => {
    if (
      pane.generation === generation &&
      pane.editor === editor &&
      context.connection === current &&
      !current.closing
    )
      pane.input.focus();
  };
  picker.reference = () => {
    picker.close();
    pane.showReferencePicker();
  };
  pane.fileDialog = picker;
  pane.invalidate();
  void picker.open("", "directory");
}

export function showHistoryPicker(pane, context) {
  const editor = pane.editor,
    generation = pane.generation,
    current = context.connection,
    history = pane.historyContext;
  if (!editor || !generation || !history || pane.switching)
    throw new Error("Open a native conversation first");
  pane.referenceDialog?.close();
  pane.historyDialog?.close();
  const call = context.boundCall(current);
  const active = () =>
    pane.generation === generation &&
    pane.editor === editor &&
    context.connection === current &&
    !current.closing;
  const picker = createHistoryPicker({
    context: history,
    workspaces: (context.view?.catalog?.workspaces ?? []).map((item) => ({
      id: item.id,
      path: item.coordinates.path,
    })),
    sources: [
      ...(context.view?.navigation?.entries ?? []).map((entry) => ({
        id: entry.session,
        title: entry.metadata.title ?? entry.session,
        kind: "native",
      })),
      ...(context.view?.external_catalog?.conversations ?? []).map((entry) => {
        const snapshot = entry.snapshot ?? entry;
        return {
          id: snapshot.id,
          title: `${snapshot.id} · observed`,
          kind: "external",
        };
      }),
    ],
    request: async (request) =>
      JSON.parse(
        await call(
          "reference_input",
          JSON.stringify({
            pane: pane.index,
            generation,
            operation: { kind: "history", request },
          }),
        ),
      ),
    release: (reference) =>
      pane.releaseReference(reference, editor, generation, call),
    add: (reference) =>
      pane.edit(editor.text, editor.images, [...editor.references, reference]),
    active,
    changed: () => pane.invalidate(),
    closed: () => {
      if (pane.historyDialog === picker) pane.historyDialog = undefined;
      pane.invalidate();
    },
  });
  picker.id = String(
    (pane.historyDialogSequence = (pane.historyDialogSequence ?? 0) + 1),
  );
  picker.restoreFocus = () => {
    if (active()) pane.input.focus();
  };
  pane.historyDialog = picker;
  pane.invalidate();
  void picker.discover(true);
}

export function showReferencePicker(pane, context, existing) {
  const editor = pane.editor,
    generation = pane.generation,
    current = context.connection;
  if (!editor || !generation || pane.switching)
    throw new Error("Open a conversation first");
  pane.historyDialog?.close();
  pane.referenceDialog?.close();
  const call = context.boundCall(current);
  const picker = createReferencePicker({
    existing,
    active: () =>
      pane.generation === generation &&
      pane.editor === editor &&
      context.connection === current &&
      !current.closing,
    request: async (operation) =>
      JSON.parse(
        await call(
          "reference_input",
          JSON.stringify({ pane: pane.index, generation, operation }),
        ),
      ),
    release: (reference) =>
      pane.releaseReference(reference, editor, generation, call),
    add: (reference) =>
      pane.edit(editor.text, editor.images, [...editor.references, reference]),
    canAdd: (reference) =>
      !editor.references.some(
        (item) => item.snapshot.sha256 === reference.snapshot.sha256,
      ) && editor.references.length < 4,
    describe: (meta) => {
      const interval = context.referenceInterval(meta);
      return {
        origin: `${context.referenceOrigin(meta)} · through record ${interval.through_seq}`,
        retained: `Retained records ${interval.first}–${interval.last} · ${meta.text_bytes} text bytes`,
        omissions: interval.omissions.length
          ? `Earlier material omitted: ${interval.omissions.map((reason) => ({ fact_limit: "capture reached 1,024 Facts", scan_bytes: "capture reached 16 MiB of source Facts", content_bytes: "text reached 1 MiB" })[reason]).join("; ")}.`
          : undefined,
      };
    },
    changed: () => pane.invalidate(),
    closed: () => {
      if (pane.referenceDialog === picker) pane.referenceDialog = undefined;
      pane.invalidate();
    },
  });
  picker.restoreFocus = () => {
    if (
      pane.generation === generation &&
      pane.editor === editor &&
      context.connection === current &&
      !current.closing
    )
      pane.input.focus();
  };
  picker.id = String(
    (pane.referenceDialogSequence = (pane.referenceDialogSequence ?? 0) + 1),
  );
  picker.choices = (context.view?.navigation?.entries ?? []).map((entry) => ({
    id: entry.session,
    title: entry.metadata.title ?? entry.session,
  }));
  pane.referenceDialog = picker;
  pane.invalidate();
}
