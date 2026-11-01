// @ts-check
/**
 * @param {import('../bridge.ts').View | undefined} view
 * @param {string | undefined} frameId
 * @param {import('../presentation-types.ts').PresentationFrame} frame
 * @returns {import('../bridge.ts').View | undefined}
 */
export function mergePresentationFrame(view, frameId, frame) {
  if (typeof frame.frame_id !== "string" || !/^[1-9][0-9]{0,19}$/.test(frame.frame_id) || BigInt(frame.frame_id) > 18446744073709551615n) throw new Error("Invalid presentation frame ID");
  let next;
  if (frame.kind === "snapshot") {
    next = frame.view;
  } else if (frame.kind === "patch") {
    if (!view || !frameId || frame.base_frame_id !== frameId) return undefined;
    if (BigInt(frame.frame_id) !== BigInt(frameId) + 1n) return undefined;
    next = { ...view, ...frame.sections, surfaces: { ...view.surfaces } };
    for (const change of frame.surfaces) {
      if (!Object.hasOwn(next.surfaces, change.surface)) throw new Error("Invalid pane patch");
      const previous = next.surfaces[change.surface];
      if (!previous) throw new Error("Cannot patch an empty pane");
      if (change.fields.kind && change.fields.kind !== previous.kind) throw new Error("Pane kind changed in patch");
      // Rust only patches within a generation; kind changes require a snapshot.
      const pane = /** @type {import('../presentation-types.ts').Surface} */ ({ ...previous, ...change.fields });
      if (change.transcript) {
        if (pane.kind === "external") throw new Error("External pane has no native transcript");
        const delta = change.transcript;
        const blocks = new Map(pane.transcript.blocks.map(block => [block.key, block]));
        for (const key of delta.remove) blocks.delete(key);
        for (const block of delta.upsert) blocks.set(block.key, block);
        const order = delta.order ?? pane.transcript.blocks.map(block => block.key);
        if (new Set(order).size !== order.length || order.length !== blocks.size || order.some(key => !blocks.has(key))) throw new Error("Invalid block order");
        const turns = { ...pane.transcript.turns?.entries };
        for (const key of (delta.turns?.remove ?? [])) delete turns[key];
        Object.assign(turns, delta.turns?.upsert);
        pane.transcript = { ...pane.transcript, ...delta.fields, turns: { revision: delta.turns?.revision ?? pane.transcript.turns?.revision, entries: turns }, blocks: order.map(key => {const block=blocks.get(key);if(!block) throw new Error("Missing block");return block;}) };
      }
      next.surfaces[change.surface] = pane;
    }
  } else { throw new Error("Unknown presentation frame"); }
  if (!next?.surfaces || Array.isArray(next.surfaces) || Object.keys(next.surfaces).length > 2 || Object.keys(next.surfaces).some(key => !/^[a-z0-9_-]{1,32}$/.test(key))) throw new Error("Invalid presentation snapshot");
  return next;
}
