import { run } from "../bridge.ts";

interface SavedDraft {
  id: string;
  summary: string;
  expired?: boolean;
  canRecreate?: boolean;
  empty: boolean;
  unsaved?: {
    text: string;
    message: string;
    resolve(saved: boolean): Promise<void>;
  };
  open(): Promise<void>;
  recreate(): Promise<void>;
  remove(): Promise<void>;
}
export interface SavedDraftsState {
  rows: SavedDraft[];
  busy: boolean;
  close(): void;
}
export function SavedDrafts({ state }: { state?: SavedDraftsState }) {
  return (
    <div className="draft-recovery" hidden={!state}>
      {state && (
        <>
          <p className="hint">Saved drafts for this device</p>
          <button type="button" className="quiet" onClick={() => state.close()}>
            Close saved drafts
          </button>
          {state.rows.map((row) => (
            <div key={row.id} className="saved-draft">
              <span>{row.summary}</span>
              <button
                type="button"
                className="quiet"
                disabled={state.busy}
                onClick={() => void run(row.open)}
              >
                Open saved conversation
              </button>
              {row.expired && (
                <p className="hint">
                  The original Session is no longer available. Your input is
                  still saved.
                </p>
              )}
              {row.canRecreate && (
                <button
                  type="button"
                  className="quiet recreate"
                  disabled={state.busy}
                  onClick={() => void run(row.recreate)}
                >
                  Start a new conversation with this draft
                </button>
              )}
              {row.unsaved && (
                <>
                  <textarea
                    readOnly
                    rows={3}
                    value={row.unsaved.text}
                    aria-label="Unsaved recovered input"
                  />
                  <p className="draft-error">
                    Input is not saved. {row.unsaved.message}
                  </p>
                  <button
                    type="button"
                    className="quiet"
                    disabled={state.busy}
                    onClick={() => void run(() => row.unsaved!.resolve(true))}
                  >
                    Use saved input
                  </button>
                  <button
                    type="button"
                    className="quiet"
                    disabled={state.busy}
                    onClick={() => void run(() => row.unsaved!.resolve(false))}
                  >
                    Replace saved text and images
                  </button>
                </>
              )}
              {row.empty && (
                <button
                  type="button"
                  className="quiet"
                  disabled={state.busy}
                  onClick={() => void run(row.remove)}
                >
                  Remove empty draft
                </button>
              )}
            </div>
          ))}
          {!state.rows.length && <p className="hint">No saved drafts</p>}
        </>
      )}
    </div>
  );
}
