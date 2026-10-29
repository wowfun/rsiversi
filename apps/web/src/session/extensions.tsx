import { run } from "../bridge.ts";
import type {
  ProjectionActions,
  SessionProjections,
} from "./projection-types.ts";
export function SessionExtensions({
  data,
}: {
  data?: SessionProjections | null;
}) {
  const snapshot = data?.projections;
  return (
    <details
      className="session-extensions"
      aria-label="Extension state"
      hidden={!data}
    >
      <summary>Extensions</summary>
      <p className="hint">
        {data?.projection_notice
          ? "Unavailable · last snapshot"
          : snapshot
            ? snapshot.cursor.kind === "draft"
              ? "Draft state"
              : "Recorded session state"
            : "Loading"}
      </p>
      {snapshot && (
        <details className="projection-coordinates">
          <summary>Snapshot details</summary>
          <pre>{JSON.stringify(snapshot.cursor, null, 2)}</pre>
        </details>
      )}
      {data?.projection_notice && (
        <p className="extension-error">{data.projection_notice}</p>
      )}
      {snapshot?.entries.map((entry) => (
        <section
          key={entry.producer}
          className="extension-value"
          data-producer={entry.producer}
        >
          <strong>{entry.producer}</strong>
          <pre
            className={
              entry.content.kind === "failed" ? "extension-error" : undefined
            }
          >
            {entry.content.kind === "failed"
              ? `Producer failed: ${entry.content.message}`
              : JSON.stringify(entry.content.value, null, 2)}
          </pre>
        </section>
      ))}
      {snapshot && !snapshot.entries.length && (
        <p>No extension views in this preset</p>
      )}
    </details>
  );
}

export function SurfaceMenu({
  data,
  owner,
  remote,
}: {
  data?: SessionProjections | null;
  owner: ProjectionActions;
  remote?: boolean;
}) {
  return (
    <span className="ui-menu">
      {data?.ui_surfaces?.map((surface) => (
        <button
          key={JSON.stringify(surface.reference)}
          type="button"
          className="quiet"
          onClick={() =>
            void run(() =>
              owner.action("ui_surface", { reference: surface.reference }),
            )
          }
        >
          {surface.title}
        </button>
      ))}
      {data && remote && (
        <button
          type="button"
          className="quiet"
          onClick={() => void run(() => owner.action("remote_ui_list"))}
        >
          Service extensions
        </button>
      )}
    </span>
  );
}
