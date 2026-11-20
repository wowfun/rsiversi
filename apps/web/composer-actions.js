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
