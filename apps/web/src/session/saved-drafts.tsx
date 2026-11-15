import { run } from "../bridge.ts";
import { useRef } from "react";

interface SavedDraft {
  id: string;
  summary: string;
  text: string;
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
  cleanup?: () => Promise<void>;
  retireReferences?: () => Promise<void>;
}
export interface SavedDraftsState {
  rows: SavedDraft[];
  busy: boolean;
  close(): void;
}
export function SavedDrafts({ state }: { state?: SavedDraftsState }) {
  const panel = useRef<HTMLDivElement>(null);
  const act = (action: () => Promise<void>, button: HTMLButtonElement) => void run(async () => {
    await action();
    requestAnimationFrame(() => {
      if (!button.isConnected && document.activeElement === document.body)
        panel.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    });
  });
  return (
    <div ref={panel} className="draft-recovery" hidden={!state}>
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
                aria-label={`Open saved conversation: ${row.summary}`}
                onClick={event => act(row.open, event.currentTarget)}
              >
                Open saved conversation
              </button>
              {row.expired && (
                <>
                <p className="hint">
                  The original Session is no longer available. Your input is
                  available below.
                </p>
                <textarea readOnly rows={4} value={row.text} aria-label="Expired conversation input" />
                {row.retireReferences && <button type="button" className="quiet" disabled={state.busy}
                  onClick={event => act(row.retireReferences!, event.currentTarget)}>Remove expired references</button>}
                </>
              )}
              {row.cleanup && (
                <button type="button" className="quiet" disabled={state.busy}
                  aria-label={`Clean up references: ${row.summary}`}
                  onClick={event => act(row.cleanup!, event.currentTarget)}>
                  Clean up references
                </button>
              )}
              {row.canRecreate && (
                <button
                  type="button"
                  className="quiet recreate"
                  disabled={state.busy}
                  aria-label={`Start a new conversation with this draft: ${row.summary}`}
                  onClick={event => act(row.recreate, event.currentTarget)}
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
                    aria-label={`Use saved input: ${row.summary}`}
                    onClick={event => act(() => row.unsaved!.resolve(true), event.currentTarget)}
                  >
                    Use saved input
                  </button>
                  <button
                    type="button"
                    className="quiet"
                    disabled={state.busy}
                    aria-label={`Replace saved text and images: ${row.summary}`}
                    onClick={event => act(() => row.unsaved!.resolve(false), event.currentTarget)}
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
                  aria-label={`Remove empty draft: ${row.summary}`}
                  onClick={event => act(row.remove, event.currentTarget)}
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
