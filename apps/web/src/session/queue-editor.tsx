import css from "./surface.module.css";
import { useLayoutEffect, useRef } from "react";
import { useOwnedDialog } from "./use-owned-dialog.ts";
import { run } from "../bridge.ts";
import { fileSizeText } from "../../vendor/dsh/primitives/file-size.ts";
import type {QueuedBlock} from "./projection-types.ts";

function attachmentLabel(block: Exclude<QueuedBlock,{type:"text"}>): string {
  switch (block.type) {
    case "file": return `${block.file.name} · ${fileSizeText(block.file.bytes)}`;
    case "image": return "Attached image retained";
    case "reference": return `Conversation reference retained: ${block.reference.preview}`;
  }
}

export interface QueueEditorState {
  id: string;
  inlineSlot?: string;
  restoreFocus(): void;
  editable: boolean;
  saving: boolean;
  status: string;
  blocks: {
    key: string;
    value: QueuedBlock;
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
              <p className="hint">{attachmentLabel(value)}</p>
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
