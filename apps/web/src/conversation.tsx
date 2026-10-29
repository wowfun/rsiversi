import css from "./conversation.module.css";
import {workspaceName} from '../workspace-name.js';
import { SessionSurfaces } from "./session/surface.tsx";
import { useState, useLayoutEffect } from "react";
import { useLayout } from "./presentation.tsx";
import { Button } from "./button.tsx";
import { input, run, useView, useSelected } from "./bridge.ts";
import { openTerminals } from "./resource-dock.tsx";
export function Conversation() {
  const [column, setColumn] = useState<HTMLElement | null>(null);
  const surfaceKeys = useView((view) =>
    Object.keys(view?.surfaces ?? {}).join(","),
  );
  const selected = useSelected((value) => value);
  const { layout, update } = useLayout();
  const mode = layout.detail;
  useLayoutEffect(() => {
    document.documentElement.dataset.detail = mode;
    window.dispatchEvent(new Event("rsi:detail-mode"));
  }, [mode]);
  const surface = useView((view) => view?.surfaces[selected]);
  const title = useView(view => {
    const active = view?.surfaces[selected];
    const entries = [...(view?.navigation?.entries ?? []), ...Object.values(view?.navigation?.groups ?? {}).flatMap(group=>group.entries)];
    const workspace=view?.catalog.workspaces.find(entry=>entry.id===active?.workspace);
    return entries.find(entry=>entry.session===active?.session)?.metadata.title || workspaceName(active?.path,workspace?.name);
  });
  return (
    <section
      ref={setColumn}
      className={`workspace-main mode-${mode} ${css.scope}`}
      aria-label="Conversations"
    >
      <div className="session-toolbar">
        <nav className="pane-tabs" aria-label="Conversation surfaces">
          {surfaceKeys
            .split(",")
            .filter(Boolean)
            .map((key) => (
              <span key={key}>
                <Button
                  size="sm"
                  id={`pane-tab-${key}`}
                  aria-pressed={selected === key}
                  onClick={() => input.select(key)}
                >
                  {key === "main"
                    ? (title || "New conversation")
                    : key === "compare"
                      ? "Compare"
                      : key}
                </Button>
                {key !== "main" && (
                  <Button
                    size="sm"
                    aria-label={`Close ${key}`}
                    onClick={() => run(() => input.close(key))}
                  >
                    ×
                  </Button>
                )}
              </span>
            ))}
        </nav>
        {surfaceKeys.split(",").filter(Boolean).length > 1 && (
          <label className="mobile-surface">
            Conversation
            <select
              aria-label="Active conversation"
              value={selected}
              onChange={(event) => input.select(event.target.value)}
            >
              {surfaceKeys
                .split(",")
                .filter(Boolean)
                .map((key) => (
                  <option key={key} value={key}>
                    {key === "main"
                      ? "Conversation"
                      : key === "compare"
                        ? "Compare"
                        : key}
                  </option>
                ))}
            </select>
          </label>
        )}
        {selected !== "main" && (
          <Button
            className="mobile-surface"
            size="sm"
            aria-label={`Close ${selected}`}
            onClick={() => run(() => input.close(selected))}
          >
            Close conversation surface
          </Button>
        )}
        <details className="conversation-menu">
          <summary aria-label="Conversation options">···</summary>
          <div className="conversation-menu-items">
            <label>
              Detail level
              <select
                aria-label="Detail level"
                value={mode}
                onChange={(event) =>
                  update({ detail: event.target.value as typeof mode })
                }
              >
                {["compact", "standard", "detailed", "verbose"].map((mode) => (
                  <option key={mode} value={mode}>
                    {mode[0].toUpperCase() + mode.slice(1)}
                  </option>
                ))}
              </select>
            </label>
            {surfaceKeys.split(",").filter(Boolean).length < 2 && (
              <Button
                size="sm"
                id="add-surface"
                onClick={() => run(() => input.add("compare"))}
              >
                Compare
              </Button>
            )}
            <Button
              size="sm"
              disabled={!surface || surface.kind === "external"}
              onClick={() => run(() => input.restoreDrafts())}
            >
              Restore drafts
            </Button>
            <Button
              size="sm"
              onClick={() => update({ conversationWidth: null })}
            >
              Automatic conversation width
            </Button>
            <Button
              size="sm"
              disabled={surface?.kind === "external" || surface?.protected}
              onClick={openTerminals}
            >
              Terminal
            </Button>
          </div>
        </details>
        <div
          id="connection-controls-host"
          className="connection-controls-host"
        />
      </div>
      <div id="panes" className="panes">
        <SessionSurfaces mode={mode} column={column} />
      </div>
    </section>
  );
}
