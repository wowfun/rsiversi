import css from "./surface.module.css";
import { useState } from "react";
import { useOwnedDialog } from "./use-owned-dialog.ts";
import { run } from "../bridge.ts";

export interface ReferencePickerState {
  id: string;
  restoreFocus(): void;
  existing: boolean;
  capturing: boolean;
  status: string;
  addDisabled: boolean;
  choices: { id: string; title: string }[];
  preview?: {
    origin: string;
    retained: string;
    omissions?: string;
    text: string;
    previous?: number;
    next?: number;
  };
  capture(source: string): Promise<void>;
  page(offset: number): Promise<void>;
  add(): void;
  close(): void;
}
export function ReferencePicker({ state }: { state: ReferencePickerState }) {
  const dialog = useOwnedDialog(state, "input"),
    [source, setSource] = useState("");
  const title = state.existing
      ? "Frozen conversation reference"
      : "Reference a conversation",
    preview = state.preview;
  return (
    <dialog
      ref={dialog}
      className={`detail-dialog reference-dialog ${css.scope}`}
      aria-label={title}
      onClose={() => {
        if (!dialog.current?.open) state.close();
      }}
    >
      <div className="dialog-heading">
        <h2>{title}</h2>
        <button type="button" className="quiet" onClick={() => state.close()}>
          Close
        </button>
      </div>
      <p className="hint">
        Capture human and assistant text from a saved conversation. The
        reference keeps these bytes when the source changes.
      </p>
      {!state.existing && (
        <>
          <select
            aria-label="Recent source conversations"
            value={
              state.choices.some((choice) => choice.id === source) ? source : ""
            }
            onChange={(event) => setSource(event.target.value)}
          >
            <option value="">Choose a visible conversation…</option>
            {state.choices.map((choice) => (
              <option key={choice.id} value={choice.id}>
                {choice.title}
              </option>
            ))}
          </select>
          <input
            type="text"
            placeholder="Session ID"
            aria-label="Source Session ID"
            maxLength={256}
            value={source}
            onChange={(event) => setSource(event.target.value)}
          />
          <div className="actions">
            <button
              type="button"
              className="primary"
              disabled={state.capturing}
              onClick={() => void state.capture(source)}
            >
              Capture preview
            </button>
          </div>
        </>
      )}
      <p className="hint" role="status">
        {state.status}
      </p>
      <section className="reference-preview-content">
        {preview && (
          <>
            <p className="reference-origin">{preview.origin}</p>
            <p className="hint">{preview.retained}</p>
            {preview.omissions && <p className="hint">{preview.omissions}</p>}
            <pre>{preview.text}</pre>
            {preview.previous !== undefined && (
              <button
                type="button"
                className="quiet"
                onClick={() => void state.page(preview.previous!)}
              >
                Previous reference page
              </button>
            )}
            {preview.next !== undefined && (
              <button
                type="button"
                className="quiet"
                onClick={() => void state.page(preview.next!)}
              >
                Next reference page
              </button>
            )}
            {!state.existing && (
              <button
                type="button"
                className="primary"
                disabled={state.addDisabled}
                onClick={() => void run(async () => state.add())}
              >
                Add to draft
              </button>
            )}
          </>
        )}
      </section>
    </dialog>
  );
}
