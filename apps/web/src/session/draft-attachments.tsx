import { run } from "../bridge.ts";
import type { ComposerOwner, ComposerView } from "./composer-state.ts";
type Props = { owner: ComposerOwner; state?: ComposerView };
export function DraftReferences({ owner, state }: Props) {
  return (
    <>
      <div
        className="draft-references"
        aria-label="Conversation references"
        hidden={!state?.references.length}
      >
        {state?.references.map((reference) => (
          <div key={reference.id} className="reference-row">
            <span className="reference-origin">{reference.label}</span>
            <button
              type="button"
              className="quiet"
              onClick={() => void run(() => owner.previewReference(reference.id))}
            >
              Preview reference
            </button>
            <button
              type="button"
              className="quiet"
              onClick={() =>
                void run(() => owner.removeReference(reference.id))
              }
            >
              Remove reference
            </button>
          </div>
        ))}
      </div>
    </>
  );
}
export function DraftImages({ owner, state }: Props) {
  return (
    <>
      <div className="draft-images">
        {state?.images.map((media, index) => (
          <div key={index} className="draft-image">
            <span>
              {index + 1}. {media.width} × {media.height} · {media.bytes} bytes
            </span>
            <button
              type="button"
              className="quiet"
              onClick={() => void run(() => owner.previewImage(index))}
            >
              Preview image
            </button>
            <button
              type="button"
              className="quiet"
              disabled={index === 0}
              onClick={() => void run(() => owner.moveImage(index, index - 1))}
            >
              Move image earlier
            </button>
            <button
              type="button"
              className="quiet"
              disabled={index + 1 === state.images.length}
              onClick={() => void run(() => owner.moveImage(index, index + 1))}
            >
              Move image later
            </button>
            <button
              type="button"
              className="quiet"
              onClick={() => void run(() => owner.moveImage(index, null))}
            >
              Remove image
            </button>
          </div>
        ))}
      </div>
      <p className="hint frozen-images">
        {state?.pending?.images
          ? `Previous submission retains ${state.pending.images} image(s) in its original order. Draft changes apply to the next submission.`
          : ""}
      </p>
    </>
  );
}
