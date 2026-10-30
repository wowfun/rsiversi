import { useLayoutEffect, useRef } from "react";
import type { Model } from "../bridge.ts";
import {ComposerAdd, ComposerAccess, ComposerModel} from "./composer-controls.tsx";
import { Completions, ResourcePreview } from "./completions.tsx";
import type { ComposerOwner, ComposerProjection } from "./composer-state.ts";
import { DraftReferences, DraftImages } from "./draft-attachments.tsx";
import { DraftStatus } from "./draft-status.tsx";
import {
  IconSendOutlineMedium,
} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";

type Register = (name: string) => (node: HTMLElement | null) => void;

export function Composer({
  owner,
  data,
  models,
  submitKey,
  register,
  hintId,
}: {
  owner: ComposerOwner;
  data?: ComposerProjection | null;
  models: Model[];
  submitKey: string;
  register: Register;
  hintId: string;
}) {
  const state = owner.composerView,
    input = useRef<HTMLTextAreaElement | null>(null),
    delivery = useRef<HTMLDetailsElement | null>(null);
  const label = owner.index === "compare" ? "Compare" : "Main";
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
      <DraftReferences owner={owner} state={state} />
      <textarea
        ref={(node) => {
          input.current = node;
          register("input")(node);
        }}
        disabled={state?.disabled ?? true}
        aria-label={`${label} message`}
        aria-describedby={hintId}
        aria-expanded={!!owner.completions}
        aria-autocomplete="list"
        aria-controls={
          owner.completions ? `completion-${owner.index}` : undefined
        }
        aria-activedescendant={
          owner.completions?.entries?.[owner.completions.selected]
            ? `completion-${owner.index}-${owner.completions.selected}`
            : undefined
        }
        placeholder={
          data
            ? "Send a message, / for commands, @ for files or conversations"
            : "Describe what you want to build, / for commands, @ for files or conversations"
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
      <div className="composer-bar">
        <ComposerAdd owner={owner} data={data} register={register}/>
        <ComposerAccess owner={owner}/>
        <ComposerModel owner={owner} data={data} models={models}/>
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
    </form>
  );
}
