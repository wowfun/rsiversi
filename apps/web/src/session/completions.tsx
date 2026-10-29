import { Fragment, useLayoutEffect, useRef } from "react";
import { run } from "../bridge.ts";
import type {
  ProjectionActions,
  SessionProjections,
  Register,
} from "./projection-types.ts";
export function ResourcePreview({
  data,
  owner,
  register,
}: {
  data?: SessionProjections | null;
  owner: ProjectionActions;
  register: Register;
}) {
  const value = data?.resource?.value;
  return (
    <section
      ref={register}
      className="input-resource-preview"
      hidden={value?.kind !== "read"}
    >
      {value?.kind === "read" && (
        <>
          <div className="resource-preview-title">
            <strong>{value.resource.name}</strong>
            <button
              type="button"
              className="quiet"
              onClick={() =>
                void run(async () => {
                  await owner.action("resource_close");
                  owner.focusInput();
                })
              }
            >
              Close preview
            </button>
          </div>
          <p className="hint">{value.resource.source}</p>
          <pre className="resource-preview-text">{value.text}</pre>
        </>
      )}
    </section>
  );
}

export function Completions({
  owner,
  index,
  register,
}: {
  owner: ProjectionActions;
  index: string;
  register: Register;
}) {
  const state = owner.completions,
    list = useRef<HTMLDivElement>(null);
  const entries = state?.entries,
    selected = entries?.[state?.selected ?? 0];
  useLayoutEffect(() => {
    list.current
      ?.querySelector('[aria-selected="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [state?.selected]);
  return (
    <section
      ref={register}
      className="input-completions"
      aria-label="Commands, skills and agents"
      hidden={!state}
    >
      {state && (
        <>
          <div
            ref={list}
            className="completion-options"
            id={`completion-${index}`}
            role="listbox"
          >
            {entries?.map((item, i) => (
              <Fragment key={`${item.group}:${item.replacement}`}>
                {entries[i - 1]?.group !== item.group && (
                  <div className="completion-group" role="presentation">
                    {item.group === "agent"
                      ? "Agents"
                      : item.group === "skill"
                        ? "Skills"
                        : "Commands"}
                  </div>
                )}
                <button
                  type="button"
                  id={`completion-${index}-${i}`}
                  className="completion-option"
                  tabIndex={-1}
                  role="option"
                  aria-selected={state.selected === i}
                  onMouseDown={(event) => event.preventDefault()}
                  onClick={() => void run(() => owner.pickCompletion(item))}
                >
                  {item.replacement}
                  <span className="completion-description">
                    {item.description}
                  </span>
                </button>
              </Fragment>
            ))}
          </div>
          <p
            className="hint"
            role="status"
            hidden={!!entries?.length && !state.failure}
          >
            {state.failure ??
              (entries ? "No matching entries" : "Loading entries…")}
          </p>
          <div className="completion-tools">
            <span className="hint">
              ↑ ↓ choose · Enter / Tab insert · F2 preview · Esc close
            </span>
            <button
              type="button"
              className="quiet"
              hidden={!selected?.resource}
              onClick={() =>
                void run(() =>
                  owner.action("resource_read", {
                    request: selected?.resource,
                  }),
                )
              }
            >
              {selected?.group === "agent" ? "Preview agent" : "Preview skill"}
            </button>
            <button
              type="button"
              className="quiet"
              onClick={() => owner.updateCompletions(true)}
            >
              Refresh
            </button>
          </div>
          {state.notice && <p className="hint">{state.notice}</p>}
        </>
      )}
    </section>
  );
}
