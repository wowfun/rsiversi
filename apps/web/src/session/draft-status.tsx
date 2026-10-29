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
      {state?.storageNotice && (
        <>
          {state.discardConfirmed && (
            <p className="draft-error">
              This deletes all saved text, images, references and pending
              request identities for this site. It cannot be undone.
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
      {state?.failure && (
        <>
          <p className="draft-error">
            Input is not saved. {state.failure.message}
          </p>
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
      {pending && (
        <>
          <p className="hint">
            {pending.kind === "command"
              ? "Command"
              : pending.kind === "queue"
                ? "Queue edit"
                : "Message"}{" "}
            {pending.id} ·{" "}
            {pending.phase === "prepared"
              ? "Saved before execution"
              : "Awaiting confirmation"}
          </p>
          {!!pending.references && (
            <p className="hint">
              Saved request includes {pending.references} frozen conversation
              reference{pending.references === 1 ? "" : "s"}. Later draft edits
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
