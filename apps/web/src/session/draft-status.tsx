import { run } from "../bridge.ts";
import type { ComposerOwner, ComposerView } from "./composer-state.ts";
export function DraftStatus({
  owner,
  state,
}: {
  owner: ComposerOwner;
  state?: ComposerView;
}) {
  const pending = state?.pending;
  return (
    <div className="draft-status">
      {state?.bindingError && (
        <p className="draft-error">{state.bindingError}</p>
      )}
      <p className="draft-error" role="status" aria-live="polite" aria-atomic="true">
        {state?.storageNotice}
      </p>
      {(state?.storageNotice || state?.discardConfirmed) && (
        <>
          {state.discardConfirmed && (
            <p className="draft-error">
              This deletes all saved drafts for this site, including drafts that
              have not started a conversation, and their text, images, references and pending
              request identities. A pending request may already
              have run; discarding its identity does not undo it. This cannot be undone.
            </p>
          )}
          <button
            type="button"
            className="quiet"
            disabled={state.discarding}
            onClick={() => void run(() => owner.discardSavedDrafts())}
          >
            {state.discardConfirmed
              ? "Confirm discard all saved drafts"
              : "Discard saved drafts…"}
          </button>
        </>
      )}
      {state?.cleanups.map((cleanup) => (
        <div key={cleanup.id} className="draft-error">
          <p>{cleanup.message}</p>
          <button
            type="button"
            className="quiet"
            onClick={() => void run(cleanup.retry)}
          >
            Retry reference cleanup
          </button>
        </div>
      ))}
      <p className="draft-error" role="status" aria-live="polite" aria-atomic="true">
        {state?.failure && `Input is not saved. ${state.failure.message}`}
      </p>
      {state?.failure && (
        <>
          <button
            type="button"
            className="quiet"
            onClick={() => void run(() => state.failure!.resolve(true))}
          >
            Use saved input
          </button>
          <button
            type="button"
            className="quiet"
            onClick={() => void run(() => state.failure!.resolve(false))}
          >
            Replace saved text and images
          </button>
        </>
      )}
      {pending && !state?.sendBusy && (
        <>
          <p className="hint" role="status">
            {pending.kind === "command"
              ? "Command"
              : pending.kind === "queue"
                ? "Queue edit"
                : "Message"}{pending.kind !== "message" && ` ${pending.id}`} ·{" "}
            {pending.phase === "prepared"
              ? "Saved before execution"
              : "Awaiting confirmation"}
          </p>
          {pending.kind === "message" && <details className="hint">
            <summary>Delivery details</summary>
            <p>{state.deliveryError || pending.id}</p>
            {state.deliveryError && !state.deliveryError.includes(pending.id) && <p>{pending.id}</p>}
          </details>}
          {!!pending.captures.length && (
            <p className="hint">
              Saved request includes {pending.captures.length} frozen conversation
              reference{pending.captures.length === 1 ? "" : "s"}. Later draft edits
              apply to the next request.
            </p>
          )}
          <button
            type="button"
            className="quiet"
            onClick={() =>
              void run(() =>
                pending.phase === "prepared"
                  ? owner.cancelPrepared()
                  : owner.reconcile("query"),
              )
            }
          >
            {pending.phase === "prepared"
              ? "Cancel saved request"
              : "Check previous result"}
          </button>
        </>
      )}
    </div>
  );
}
