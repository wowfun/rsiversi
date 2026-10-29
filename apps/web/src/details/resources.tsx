import type {Command, SourceData, ResourceModel} from './model.ts';
import type {ResourceHost} from '../resource-hosts.ts';
import { useState, useSyncExternalStore, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { resources } from "../resource-hosts.ts";
import { run } from "../bridge.ts";
import { ImageDetail } from "./image.tsx";
import { InteractionDetail } from "./interaction.tsx";

function SourceDetail({ data, command }: {data: SourceData; command: Command}) {
  const [preview, showPreview] = useState(false),
    window = data.window;
  return (
    <div className="source-detail">
      <p className="hint">
        Fact {data.source.seq} · {data.source.field.kind}
      </p>
      {data.error ? (
        <p className="source-error">Source unavailable: {data.error}</p>
      ) : !window ? (
        <p>Loading source…</p>
      ) : (
        <>
          <p className="source-range">
            Bytes {window.start}–{window.end}
            {window.more ? " · more available" : " · end"}
          </p>
          <pre className="source-text">{window.text}</pre>
          <div className="actions">
            <Action
              command={command}
              value={{
                action: "source_page",
                ticket: data.ticket,
                forward: false,
              }}
              disabled={window.start === 0}
            >
              Previous source page
            </Action>
            <Action
              command={command}
              value={{
                action: "source_page",
                ticket: data.ticket,
                forward: true,
              }}
              disabled={!window.more}
            >
              Next source page
            </Action>
          </div>
          {data.media && (
            <>
              <button
                type="button"
                className="quiet"
                disabled={preview}
                onClick={() => showPreview(true)}
              >
                Preview source image
              </button>
              {preview && <ImageDetail data={{owner: data.owner, ticket: data.ticket, media: data.media, acquire: data.acquire}} />}
            </>
          )}
        </>
      )}
    </div>
  );
}
function Action({ command, value, disabled = false, children, className = "quiet" }: {command: Command; value: Record<string, unknown>; disabled?: boolean; children: ReactNode; className?: string}) {
  return (
    <button
      type="button"
      className={className}
      disabled={disabled}
      onClick={() => void run(() => command(value))}
    >
      {children}
    </button>
  );
}
function ResourceDetail({ model }: {model?: ResourceModel}) {
  if (!model) return null;
  const { kind, data, command } = model;
  if (kind === "image") return <ImageDetail data={data} />;
  if (kind === "interaction")
    return <InteractionDetail data={data} command={command} />;
  if (kind === "source") return <SourceDetail data={data} command={command} />;
  if (kind === "ui")
    return !data.model || !data.binding ? (
      <p className="hint">{data.error ?? "Loading…"}</p>
    ) : null;
  if (kind === "extensions")
    return (
      <div className="settings-list">
        {data.error ? (
          <p className="settings-error">{data.error}</p>
        ) : !data.page ? (
          <p>Loading service extensions…</p>
        ) : (
          <>
            {data.page.entries.map((entry) => (
              <Action
                key={`${entry.bundle}:${entry.surface}`}
                command={command}
                value={{
                  action: "remote_ui_surface",
                  ticket: data.ticket,
                  bundle: entry.bundle,
                  surface: entry.surface,
                }}
              >
                {entry.title}
              </Action>
            ))}
            {!data.page.entries.length && <p>No service extensions.</p>}
            {data.page.next && (
              <Action
                command={command}
                value={{ action: "remote_ui_next", ticket: data.ticket }}
              >
                More extensions
              </Action>
            )}
          </>
        )}
      </div>
    );
  if (kind === "block-sources")
    return (
      <div className="block-sources" role="region" aria-label="Source evidence" tabIndex={0}>
        <p className="hint">
          Sources {data.start + (data.page.length ? 1 : 0)}–
          {data.start + data.page.length} of {data.total}
        </p>
        <div className="source-list">
          {data.page.map((source, index) => (
            <Action
              key={index}
              className="quiet source-reference"
              command={command}
              value={{
                action: "inspect_source",
                pane: data.pane,
                generation: data.generation,
                source,
              }}
            >
              Fact {source.seq} · {source.field.kind}
              {source.field.index == null ? "" : ` ${source.field.index}`}
            </Action>
          ))}
        </div>
        <div className="actions">
          <Action
            command={command}
            value={{
              action: "block_sources_page",
              ticket: data.ticket,
              forward: false,
            }}
            disabled={data.start === 0}
          >
            Previous sources
          </Action>
          <Action
            command={command}
            value={{
              action: "block_sources_page",
              ticket: data.ticket,
              forward: true,
            }}
            disabled={data.start + data.page.length >= data.total}
          >
            Next sources
          </Action>
        </div>
      </div>
    );
  return null;
}
function ResourcePresentation({ host }: {host: ResourceHost}) {
  const ui =
    host.model?.kind === "ui" &&
    host.model.data.model &&
    host.model.data.binding;
  return (
    <>
      <div
        className="ui-presentation"
        hidden={!ui}
        ref={(node) => {host.rendererRoot = node;}}
      />
      <ResourceDetail key={host.key} model={host.model} />
    </>
  );
}
export function ResourcePresentations() {
  const hosts = useSyncExternalStore(
    resources.subscribe,
    resources.getSnapshot,
  );
  return hosts.map((host) =>
    createPortal(
      <ResourcePresentation host={host} />,
      host.body,
      host.entry.view,
    ),
  );
}
