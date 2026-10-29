import css from "./surface.module.css";
import { useLayoutEffect, useRef } from "react";
import { useOwnedDialog } from "./use-owned-dialog.ts";
import { run } from "../bridge.ts";

export interface QueueEditorState {
  id: string;
  restoreFocus(): void;
  editable: boolean;
  saving: boolean;
  status: string;
  blocks: {
    key: string;
    value: { type: string; text?: string; reference?: { preview: string } };
  }[];
  edit(key: string, text: string): void;
  remove(key: string): void;
  addText(): void;
  save(): Promise<void>;
  close(): void;
}

export function QueueEditor({ state }: { state: QueueEditorState }) {
  const dialog = useOwnedDialog(state, "textarea"),
    focusAdded = useRef(false);
  useLayoutEffect(() => {
    if (focusAdded.current) {
      focusAdded.current = false;
      dialog.current
        ?.querySelector<HTMLTextAreaElement>(".queue-field:last-child textarea")
        ?.focus();
    }
  }, [state.blocks]);
  return (
    <dialog
      ref={dialog}
      className={`queue-editor ${css.scope}`}
      aria-label={state.editable ? "Edit queued input" : "Pending input"}
      onClose={() => {
        if (!dialog.current?.open) state.close();
      }}
    >
      <h2>{state.editable ? "Edit queued input" : "Pending input"}</h2>
      <div className="queue-fields">
        {state.blocks.map(({ key, value }, index) => (
          <div key={key} className="queue-field">
            {value.type === "text" ? (
              <label>
                Text {index + 1}
                <textarea
                  className="queue-text"
                  aria-label={`Queued text ${index + 1}`}
                  value={value.text}
                  readOnly={!state.editable || state.saving}
                  onChange={(event) => state.edit(key, event.target.value)}
                />
              </label>
            ) : (
              <p className="hint">
                {value.type === "image"
                  ? "Attached image retained"
                  : `Conversation reference retained: ${value.reference?.preview}`}
              </p>
            )}
            {state.editable && (
              <button
                type="button"
                className="quiet"
                disabled={state.saving}
                onClick={() => state.remove(key)}
              >
                Remove block
              </button>
            )}
          </div>
        ))}
      </div>
      <p className="hint" role="status">
        {state.status}
      </p>
      <div className={`queue-editor-actions ${css.scope}`}>
        <button type="button" className="quiet" onClick={() => state.close()}>
          Close
        </button>
        {state.editable && (
          <>
            <button
              type="button"
              className="quiet"
              disabled={state.saving}
              onClick={() => {
                focusAdded.current = true;
                state.addText();
              }}
            >
              Add text
            </button>
            <button
              type="button"
              disabled={state.saving}
              onClick={() => void run(() => state.save())}
            >
              Save replacement
            </button>
          </>
        )}
      </div>
    </dialog>
  );
}
