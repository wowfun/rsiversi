import { run } from "../bridge.ts";
import {useEffect, useRef} from "react";
import {IconChevronLeftOutlineRegular, IconChevronRightOutlineRegular, IconCloseFillRegular} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
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
              disabled={state.referenceRemovalDisabled}
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
function DraftThumbnail({owner, media, generation}: {
  owner: ComposerOwner; media: ComposerView["images"][number]; generation?: string
}) {
  const ref = useRef<HTMLImageElement>(null);
  useEffect(() => {
    if (!generation) return;
    const image = ref.current!;
    let live = true;
    let interest: ReturnType<ComposerOwner["acquireDraftImage"]>;
    try { interest = owner.acquireDraftImage(media, generation); }
    catch { return; }
    image.onload = () => { image.hidden = false; interest.release(); };
    image.onerror = () => { image.hidden = true; interest.release(); };
    void interest.result.then(url => {
      if (live && url) image.src = url;
    }, () => {});
    return () => {
      live = false;
      image.onload = image.onerror = null;
      image.hidden = true;
      image.removeAttribute("src");
      interest.release();
    };
  }, [owner, generation, media.id, media.width, media.height, media.bytes]);
  return <img ref={ref} alt="" hidden />;
}

export function DraftImages({ owner, state }: Props) {
  return (
    <>
      {!!(state?.images.length || state?.importingImages) && <div className="draft-images" role="group" aria-label="Pending attachments">
        {state?.images.map((media, index) => (
          <div key={media.id + ':' + index} className="draft-image">
            <button
              type="button"
              className="draft-thumbnail"
              aria-label="Preview image"
              title={`Image ${index + 1} · ${media.width} × ${media.height} · ${media.bytes} bytes`}
              onClick={() => void run(() => owner.previewImage(index))}
            >
              <span>{media.width} × {media.height}</span>
              <DraftThumbnail owner={owner} media={media} generation={state.generation}/>
            </button>
            {state.images.length > 1 && <><button
              type="button"
              className="draft-earlier" aria-label="Move image earlier" title="Move image earlier"
              disabled={index === 0}
              onClick={() => void run(() => owner.moveImage(index, index - 1))}
            >
              <IconChevronLeftOutlineRegular size={12}/>
            </button>
            <button
              type="button"
              className="draft-later" aria-label="Move image later" title="Move image later"
              disabled={index + 1 === state.images.length}
              onClick={() => void run(() => owner.moveImage(index, index + 1))}
            >
              <IconChevronRightOutlineRegular size={12}/>
            </button></>}
            <button
              type="button"
              className="draft-remove" aria-label="Remove image" title="Remove image"
              onClick={() => void run(() => owner.moveImage(index, null))}
            >
              <IconCloseFillRegular size={12}/>
            </button>
          </div>
        ))}
        {state?.importingImages && <div role="status" aria-live="polite">
          {state.importingImages.files.slice(state.importingImages.next).map((file,index) =>
            <div key={index} className="draft-image-import">{file.name || "Image"} · Uploading…</div>
          )}
        </div>}
      </div>}
      {!!state?.pending?.images && <p className="hint frozen-images">
        Previous submission retains {state.pending.images} image(s) in its original order. Draft changes apply to the next submission.
      </p>}
    </>
  );
}
