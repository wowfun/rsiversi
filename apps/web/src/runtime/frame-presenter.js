import { mergePresentationFrame } from "./presentation-frame.js";
export function createFramePresenter(context) {
  let frameId;
  async function presentFrame(frame, assets, current = context.connection) {
    if (current && (context.connection !== current || current.closing))
      return false;
    const next = mergePresentationFrame(context.view, frameId, frame);
    if (!next) return false;
    for (const pane of context.panes.values())
      if (pane.kind !== "external") {
        pane.bindActions(undefined, undefined);
      }
    const ticket = context.presentation.begin(
      current,
      frame.frame_id,
      (error) => current.fail(error),
    );
    let renderer;
    try {
      await context.presentation.commit(
        ticket,
        () => context.render(next),
        () => [
          context.$("panes"),
          ...[...context.panes.values()].map((pane) => pane.node),
          ...context.rendererSlots.map((slot) => slot.root),
        ],
      );
      if (context.connection !== current || current.closing)
        throw new Error("Presentation owner retired");
      renderer = await ticket.wait(
        current.render(assets, context.rendererSlots),
      );
      if (context.connection !== current || current.closing)
        throw new Error("Presentation owner retired");
      if (renderer?.error) context.notify(`Renderer update failed: ${renderer.error}`);
      ticket.finish();
    } catch (error) {
      ticket.abort(error);
      throw error;
    }
    frameId = frame.frame_id;
    for (const [key, pane] of context.panes)
      if (pane.kind !== "external") {
        const data = next.surfaces[key];
        pane.bindActions(
          data?.active && !data?.protected
            ? { generation: data.generation, turn_id: data.active }
            : undefined,
          data && !data.protected
            ? { generation: data.generation, items: data.queue ?? [] }
            : undefined,
        );
      }
    return { accepted: true, renderer };
  }

  return {
    presentFrame,
    reset() {
      frameId = undefined;
    },
  };
}
