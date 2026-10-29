import { useLayoutEffect, useRef } from "react";

// A modal must leave the browser's top layer before focus can return to its owner.
export function useOwnedDialog(
  state: { restoreFocus(): void },
  initialFocus: string,
) {
  const dialog = useRef<HTMLDialogElement>(null);
  useLayoutEffect(() => {
    const node = dialog.current!;
    node.showModal();
    node.querySelector<HTMLElement>(initialFocus)?.focus();
    return () => {
      node.close();
      state.restoreFocus();
    };
  }, [state, initialFocus]);
  return dialog;
}
