import { Button } from "./button.tsx";
import { StateDot } from "../vendor/dsh/primitives/StateDot.tsx";
import { input, run, useView, useSelected } from "./bridge.ts";
export function Resources() {
  const selected = useSelected((value) => value),
    surface = useView((view) => view?.surfaces[selected]),
    remote = useView((view) => view?.has_remote_ui);
  if (!surface) return null;
  if (surface.kind === "external" && surface.external) {
    const external = surface.external,
      snapshot = external.observed.snapshot;
    return (
      <>
        <h2>External conversation</h2>
        <p className="resource-path">{snapshot.cwd}</p>
        <dl className="session-facts">
          <dt>Endpoint</dt>
          <dd>{snapshot.endpoint}</dd>
          <dt>Connection</dt>
          <dd>{external.observed.connected ? "Connected" : "Disconnected"}</dd>
          <dt>Observation</dt>
          <dd>{snapshot.status}</dd>
          <dt>Visible records</dt>
          <dd>{external.blocks.length}</dd>
        </dl>
        <p className="hint">
          Switching views keeps the agent connected. Use Close peer to end its
          connection.
        </p>
      </>
    );
  }
  const invoke = (action: string, fields = {}) =>
    input.command({
      action,
      pane: selected,
      generation: surface.generation,
      ...fields,
    });
  return (
    <>
      <div className="section-heading">
        <h2>Session resources</h2>
        <StateDot
          state={
            surface.transcript.status.toLowerCase() === "running"
              ? "ongoing"
              : "idle"
          }
        />
      </div>
      <p className="resource-path" title={surface.path}>
        {surface.path}
      </p>
      <div className="resource-buttons">
        {surface.ui_surfaces.map((item) => (
          <Button
            variant="outline"
            key={JSON.stringify(item.reference)}
            onClick={() =>
              run(() => invoke("ui_surface", { reference: item.reference }))
            }
          >
            {item.title}
          </Button>
        ))}
        <Button onClick={() => run(() => invoke("commands"))}>
          Session commands
        </Button>
        {remote && (
          <Button onClick={() => run(() => invoke("remote_ui_list"))}>
            Service extensions
          </Button>
        )}
      </div>
      <dl className="session-facts">
        <dt>Session</dt>
        <dd>{surface.session}</dd>
        <dt>Status</dt>
        <dd>{surface.transcript.status || "Ready"}</dd>
        <dt>Loaded blocks</dt>
        <dd>{surface.transcript.blocks.length}</dd>
      </dl>
    </>
  );
}
