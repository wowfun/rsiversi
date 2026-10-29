import { closeDisclosure } from "../../disclosure.js";
import { awayFromBottom } from "../../turn-presentation.js";
export function mount(pane, context, elements) {
  Object.assign(pane, elements);
  const index = pane.index;
  pane.events = new AbortController();
  const listen = (node, event, handler, options = {}) =>
    node.addEventListener(event, handler, {
      ...options,
      signal: pane.events.signal,
    });
  const action = (node, run) =>
    listen(node, "click", () => {
      closeDisclosure(node);
      context.perform(run);
    });
  listen(pane.node, "focusin", () => context.select(index));
  action(pane.history, () => pane.action("history"));
  action(pane.live, () => pane.action("live"));
  action(pane.commands, async () => { pane.commandsExpanded = true; pane.invalidate(); await pane.action("commands"); });
  pane.visibleBlocks = new context.TranscriptVisibility(pane.transcript, () =>
    pane.scheduleVisible(),
  );
  listen(
    pane.transcript,
    "scroll",
    () => {
      pane.scheduleVisible();
      pane.setReadingSummary(awayFromBottom(pane.transcript));
    },
    { passive: true },
  );
  listen(pane.transcript, "keydown", (event) => {
    if (event.key === "End") {
      pane.setReadingSummary(false);
      pane.transcript.scrollTop = pane.transcript.scrollHeight;
    }
  });
  action(pane.bottom, () => {
    pane.setReadingSummary(false);
    pane.transcript.scrollTop = pane.transcript.scrollHeight;
  });
  pane.resizeObserver = new ResizeObserver(() => {
    pane.scheduleVisible();
    pane.setReadingSummary(awayFromBottom(pane.transcript));
  });
  pane.resizeObserver.observe(pane.transcript);
  listen(pane.input, "compositionstart", () => {
    pane.composing = true;
    pane.closeCompletions();
  });
  listen(pane.input, "compositionend", () => {
    pane.composing = false;
    pane.updateCompletions();
  });
  listen(pane.input, "input", () => {
    try {
      pane.edit(pane.input.value);
      pane.updateCompletions();
    } catch (error) {
      pane.input.value = pane.editor?.text ?? pane.starting?.text ?? "";
      context.notify(error.message);
    }
  });
  listen(pane.input, "keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    if (pane.completionKey(event)) return;
    const choice = context.keyboardAction(
      event,
      pane.submitKey,
      pane.composerActions,
    );
    if (choice) {
      event.preventDefault();
      context.perform(() => pane.submit(choice));
    }
  });
  listen(pane.input, "click", () => pane.updateCompletions());
  listen(pane.input, "keyup", (event) => {
    if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key))
      pane.updateCompletions();
  });
  listen(pane.composer, "submit", (event) => {
    event.preventDefault();
    context.perform(() => pane.submit("primary"));
  });
  action(pane.modelReceipt, () => pane.action("model_refresh"));
  listen(pane.imageInput, "change", () => {
    const files = [...pane.imageInput.files];
    pane.imageInput.value = "";
    context.perform(() => pane.upload(files));
  });
  action(pane.attach, () => pane.imageInput.click());
  action(pane.addReference, () => pane.showReferencePicker());
  action(pane.exportButton, () => pane.exportSession(""));
  action(pane.searchHistory, () => pane.showHistoryPicker());
  action(pane.addFile, () => pane.showFilePicker());
  action(
    pane.cancel,
    () =>
      pane.stopBinding &&
      context.command({
        action: "cancel",
        pane: pane.index,
        ...pane.stopBinding,
      }),
  );
  action(pane.steer, () => pane.submit("alternative"));
  action(pane.send, () => pane.submit("primary"));
  action(pane.protectedExport, () => pane.exportSession(""));
}

export function unmount(pane, context) {
  pane.events?.abort();
  pane.resizeObserver?.disconnect();
  pane.visibleBlocks?.dispose();
  if (pane.visibleFrame) cancelAnimationFrame(pane.visibleFrame);
  pane.visibleFrame = undefined;
  clearTimeout(pane.completionTimer);
}
