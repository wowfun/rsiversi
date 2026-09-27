// Rust owns delivery selection; this adapter maps gestures to displayed choices.
export function keyboardAction(event, submitKey, actions) {
  if (event.isComposing || event.keyCode === 229 || event.key !== "Enter" || event.shiftKey || event.altKey) return undefined;
  const modified = event.ctrlKey || event.metaKey;
  if (submitKey === "mod_enter" && !modified) return undefined;
  return submitKey === "enter" && modified && actions?.alternative ? "alternative" : "primary";
}
