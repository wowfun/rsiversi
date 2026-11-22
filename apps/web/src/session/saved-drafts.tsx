import { run } from "../bridge.ts";
import { useRef, useState } from "react";

function InputPreview({text,label,summary}: {text:string;label:string;summary:string}) {
  const [expanded,setExpanded] = useState(false);
  const preview = text.slice(0,4096).replace(/[\uD800-\uDBFF]$/, '');
  return <>
    <textarea readOnly rows={3} value={expanded ? text : preview} aria-label={`${label}: ${summary}`}/>
    {preview.length < text.length && <button type="button" className="quiet"
      aria-label={`${expanded ? 'Collapse' : 'Show full'} input: ${summary}`}
      onClick={()=>setExpanded(!expanded)}>{expanded ? 'Collapse input' : 'Show full input'}</button>}
  </>;
}

interface SavedDraft {
  id: string;
  summary: string;
  coordinates?: string;
  text: string;
  expired?: boolean;
  canRecreate?: boolean;
  empty: boolean;
  removable?: boolean;
  starting?: boolean;
  deleteConfirmed?: boolean;
  cancelDelete?: () => void;
  unsaved?: {
    text: string;
    message: string;
    resolve(saved: boolean): Promise<void>;
  };
  open(): Promise<void>;
  recreate?(): Promise<void>;
  remove(): Promise<void>;
  cleanup?: () => Promise<void>;
  retireReferences?: () => Promise<void>;
}
export interface SavedDraftsState {
  rows: SavedDraft[];
  busy: boolean;
  loading?: boolean;
  close(): void;
  discard?: () => Promise<void>;
  more?: () => Promise<void>;
  previousSessions?: () => Promise<void>;
  nextSessions?: () => Promise<void>;
  notice?: string;
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
    <div ref={panel} className="draft-recovery" hidden={!state} aria-busy={state?.busy}>
      {state && (
        <>
          <p className="hint">Saved drafts for this device</p>
          <p className="hint">Opening switches this pane. Saved input stays available here.</p>
          <button type="button" className="quiet" onClick={() => state.close()}>
            Close saved drafts
          </button>
          {state.notice && <p role="status" className="draft-error">{state.notice}</p>}
          {state.rows.map((row) => (
            <div key={row.id} className="saved-draft" title={row.coordinates}>
              <span>{row.summary}</span>
              <button
                type="button"
                className="quiet"
                disabled={state.busy}
                aria-label={`${row.starting ? 'Start a new conversation here' : 'Open saved conversation here'}: ${row.summary}`}
                onClick={event => act(row.open, event.currentTarget)}
              >
                {row.starting ? 'Start a new conversation here' : 'Open saved conversation here'}
              </button>
              {row.expired && (
                <>
                <p className="hint">
                  The original Session is no longer available. Your input is
                  available below.
                </p>
                <InputPreview text={row.text} label="Expired conversation input" summary={row.summary}/>
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
                  onClick={event => act(row.recreate!, event.currentTarget)}
                >
                  Start a new conversation with this draft
                </button>
              )}
              {row.unsaved && (
                <>
                  <InputPreview text={row.unsaved.text} label="Unsaved recovered input" summary={row.summary}/>
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
              {(row.starting || (!row.empty && row.removable)) && <>
                <InputPreview text={row.text} label="Saved starting input" summary={row.summary}/>
                <button type="button" className="quiet" disabled={state.busy}
                  aria-label={`${row.deleteConfirmed ? 'Confirm delete' : 'Delete'} ${row.starting ? 'starting draft' : 'draft'}: ${row.summary}`}
                  onClick={event=>act(row.remove,event.currentTarget)}>
                  {`${row.deleteConfirmed ? 'Confirm delete' : 'Delete'} ${row.starting ? 'starting draft' : 'draft'}${row.deleteConfirmed ? '' : '…'}`}
                </button>
                {row.deleteConfirmed && <button type="button" className="quiet" disabled={state.busy}
                  aria-label={`Cancel deletion: ${row.summary}`} onClick={row.cancelDelete}>Cancel deletion</button>}
              </>}
            </div>
          ))}
          {state.loading && <p role="status" className="hint">Loading saved drafts…</p>}
          {!state.loading && !state.notice && !state.rows.length && <p className="hint">No saved drafts</p>}
          {state.previousSessions && <button type="button" className="quiet" disabled={state.busy}
            onClick={event=>act(state.previousSessions!,event.currentTarget)}>Previous conversation drafts</button>}
          {state.nextSessions && <button type="button" className="quiet" disabled={state.busy}
            onClick={event=>act(state.nextSessions!,event.currentTarget)}>Next conversation drafts</button>}
          {state.more && <button type="button" className="quiet" disabled={state.busy}
            onClick={event=>act(state.more!,event.currentTarget)}>Load more starting drafts</button>}
          {state.discard && <button type="button" className="quiet" disabled={state.busy}
            onClick={event=>act(state.discard!,event.currentTarget)}>Discard all saved drafts…</button>}
        </>
      )}
    </div>
  );
}
