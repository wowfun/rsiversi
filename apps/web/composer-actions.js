// Rust owns delivery selection; this adapter maps gestures to displayed choices.
export function composerKeydown(event, pane) {
  if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
  const enter = event.key === "Enter";
  const modified = event.ctrlKey || event.metaKey;
  if (enter && (event.altKey || event.getModifierState?.("AltGraph") ||
      (event.ctrlKey && event.metaKey) || (modified && event.shiftKey))) {
    event.preventDefault();
    return;
  }
  if (pane.completionKey?.(event) || !enter || event.shiftKey) return;
  if (pane.submitKey === "mod_enter" && !modified) return;
  event.preventDefault();
  if (!event.repeat) return pane.submitKey !== "mod_enter" && modified && pane.composerActions?.alternative ? "alternative" : "primary";
}

// History is the currently loaded transcript. Only a multipart or clipped
// prompt needs its original fact, and only when the user reaches that prompt.
export function recallPrompt(event, pane, context) {
  const older = event.key === "ArrowUp";
  if ((!older && event.key !== "ArrowDown") || event.defaultPrevented ||
      event.isComposing || event.keyCode === 229 || pane.composing || Date.now() < pane.composingUntil ||
      event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const {editor, input, generation} = pane;
  if (!editor || !generation || input.selectionStart !== input.value.length || input.selectionEnd !== input.value.length) return;
  const revision = editor.revision;
  const position = editor.recalled?.revision === revision ? editor.recalled : undefined;
  const original = input.value;
  if (!position && (original !== "" || !older)) return;
  const blocks = pane.lastTranscript?.blocks;
  if (!blocks) return;
  let i = position ? blocks.findIndex(block => block.key === position.key) : blocks.length;
  if (i < 0) return;
  function next() {
    for (i += older ? -1 : 1; i >= 0 && i < blocks.length; i += older ? -1 : 1) {
      const block = blocks[i];
      if (block.recall === "recorded" || (block.recall === "text" && block.text !== original)) return block;
    }
    return undefined;
  }
  function replace(block, text) {
    input.select();
    input.ownerDocument.execCommand(text ? "insertText" : "delete", false, text);
    if (input.value === text && editor.text === text) {
      editor.recalled = block && {revision: editor.revision, key: block.key};
    }
  }
  let block = next();
  if (!block && older) return;
  event.preventDefault();
  if (block?.recall !== "recorded") {
    replace(block, block?.text ?? "");
    return;
  }
  if (editor.readingPrompt) return;
  editor.readingPrompt = true;
  context.perform(async () => {
    try {
      while (block?.recall === "recorded") {
        const text = await context.call("recall_prompt", {pane:pane.index, generation, key:block.key});
        if (pane.generation !== generation || pane.editor !== editor || editor.revision !== revision ||
            input.ownerDocument.activeElement !== input || input.selectionStart !== original.length ||
            input.selectionEnd !== original.length || input.readOnly ||
            !pane.lastTranscript?.blocks.some(item => item.key === block.key)) return;
        if (text !== original) { replace(block, text); return; }
        block = next();
      }
      if (block || !older) replace(block, block?.text ?? "");
    } finally {
      editor.readingPrompt = false;
    }
  });
}
