import {DocumentContributions} from "../document-plugins.tsx";
import { useId, useLayoutEffect, useRef, useState } from "react";
import {run} from "../bridge.ts";
import {ComposerAccess, ComposerModel} from "./composer-controls.tsx";
import {ComposerAdd} from "./composer-add.tsx";
import css from "./composer-menu.module.css";
import {observeControlRow} from "../../vendor/dsh/control-row-layout.ts";
import { Completions, ResourcePreview } from "./completions.tsx";
import type { ComposerOwner, ComposerProjection } from "./composer-state.ts";
import { DraftReferences, DraftImages } from "./draft-attachments.tsx";
import { DraftStatus } from "./draft-status.tsx";
import {DropOverlay} from "../../vendor/dsh/attachment/DropOverlay.tsx";
import {fileSizeText} from "../../vendor/dsh/primitives/file-size.ts";
import {bindImageIntake} from "./image-intake.ts";
import {
  IconSendOutlineMedium,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";

type Register = (name: string) => (node: HTMLElement | null) => void;

export function Composer({
  owner,
  data,
  submitKey,
  register,
  hintId,
}: {
  owner: ComposerOwner;
  data?: ComposerProjection | null;
  submitKey: string;
  register: Register;
  hintId: string;
}) {
  const state = owner.composerView,
    input = useRef<HTMLTextAreaElement | null>(null),
    row = useRef<HTMLDivElement>(null),
    delivery = useRef<HTMLDetailsElement | null>(null);
  const label = owner.index === "compare" ? "Compare" : "Main";
  const addListId = useId();
  const [dragging, setDragging] = useState(false);
  useLayoutEffect(() => input.current ? bindImageIntake(input.current, owner, setDragging) : undefined, [owner]);
  useLayoutEffect(() => row.current ? observeControlRow(row.current) : undefined, []);
  const [addChoice, setAddChoice] = useState<number | null>(null);
  // Keep the browser's caret/composition state when the durable editor text is unchanged.
  useLayoutEffect(() => {
    if (input.current) {
      if (input.current.value !== (state?.text ?? "")) input.current.value = state?.text ?? "";
      input.current.style.height = "auto";
      input.current.style.height = `${Math.min(180, Math.max(52, input.current.scrollHeight))}px`;
    }
  }, [state?.text]);
  useLayoutEffect(() => {
    if (!state?.alternative && delivery.current) delivery.current.open = false;
  }, [!!state?.alternative]);
  const modelPending = !!data?.model_command?.pending;
  const control = (
    name: string,
    text: string,
    disabled = false,
    hidden = false,
  ) => (
    <button
      ref={register(name)}
      type="button"
      className="quiet"
      disabled={disabled}
      hidden={hidden}
    >
      {text}
    </button>
  );
  return (
    <form
      ref={register("composer")}
      className="composer"
      hidden={data?.protected}
    >
      {dragging && state?.mediaLimits && <DropOverlay disabled={state.imageDisabled} labels={state.imageDisabled
        ? {title:"Images cannot be added right now"}
        : {title:"Drag images here to add them", desc:`Up to ${state.mediaLimits.images} images, ${fileSizeText(state.mediaLimits.upload_bytes)} each`}} />}
      <ResourcePreview
        data={data}
        owner={owner}
        register={register("resourcePreview")}
      />
      <Completions
        owner={owner}
        index={owner.index}
        register={register("completionPanel")}
      />
      <DocumentContributions seat="composer"/>
      <DraftReferences owner={owner} state={state} />
      <div className="composer-command">
        <span role="status" aria-live="polite" aria-atomic="true">{state?.command && <><strong>/{state.command.name}</strong> {state.command.hint}</>}</span>
        {state?.command && <button type="button" className="quiet" disabled={state.disabled} aria-label="Leave command input" onClick={()=>void run(async()=>owner.clearCommand())}>×</button>}
      </div>
      <textarea
        ref={(node) => {
          input.current = node;
          register("input")(node);
        }}
        readOnly={state?.bindingInput}
        disabled={(state?.disabled ?? true) && !state?.bindingInput}
        aria-label={state?.command ? `${label} command arguments` : `${label} message`}
        aria-describedby={hintId}
        aria-expanded={addChoice !== null || !!owner.completions}
        aria-autocomplete="list"
        aria-controls={
          addChoice !== null ? addListId : owner.completions ? `completion-${owner.index}` : undefined
        }
        aria-activedescendant={
          addChoice !== null && addChoice >= 0 ? `${addListId}-${addChoice}` : owner.completions?.entries?.[owner.completions.selected]
            ? `completion-${owner.index}-${owner.completions.selected}`
            : undefined
        }
        placeholder={
          data
            ? state?.completionsAvailable ? "Send a message · / commands · @ agents" : "Send a message"
            : "Describe what you want to build"
        }
        maxLength={1024 * 1024}
      />
      <input
        ref={register("imageInput")}
        type="file"
        accept="image/*"
        multiple
        hidden
        aria-label={`${label} image files`}
      />
      <DraftImages owner={owner} state={state} />
      <DraftStatus owner={owner} state={state} />
            <p id={hintId} className="composer-hint sr-only">
              {submitKey === "enter"
                ? "Enter to send · Shift Enter for a new line"
                : "Ctrl / ⌘ Enter to send · Enter for a new line"}
            </p>
      <div ref={row} className="composer-bar">
        <div className={css.tools}>
          <ComposerAdd owner={owner} data={data} id={addListId} choice={addChoice} choose={setAddChoice}/>
          <ComposerAccess owner={owner}/>
        </div>
        <div className={css.submission}>
          <ComposerModel owner={owner} data={data}/>
          {control("modelReceipt", "Check model change", false, !modelPending)}
          <div className="actions">
            {control("cancel", "Stop", !owner.stopBinding, !owner.stopBinding)}
            <details
              ref={delivery}
              className="delivery-menu"
              hidden={!state?.alternative}
            >
              <summary className="quiet" aria-label="Sending options">
                Sending options
              </summary>
              {control(
                "steer",
                state?.alternative?.label ?? "Steer",
                state?.alternativeDisabled ?? true,
              )}
            </details>
            <button
              ref={register("send")}
              type="button"
              className="primary"
              aria-label={state?.sendLabel ?? "Send"}
              title={state?.sendLabel ?? "Send"}
              disabled={state?.sendDisabled ?? true}
              data-testid="composer-send"
            >
              {(state?.sendLabel ?? "Send") === "Send" ? (
                <IconSendOutlineMedium />
              ) : (
                state?.sendLabel
              )}
            </button>
          </div>
        </div>
      </div>
    </form>
  );
}
