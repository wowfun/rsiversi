// @ts-check
/** @typedef {import('../bridge.ts').View} View */
/** @typedef {import('../session/state.ts').SessionSurfaceOwner} NativeOwner */
/** @typedef {import('../session/external.tsx').ExternalOwner} ExternalOwner */
/** @param {{rendererSlots: unknown[], view?: View, lastNotice?: string,
 * notify(message?: string): void, panes: Map<string, NativeOwner | ExternalOwner>,
 * ExternalPane: new(key: string) => ExternalOwner, Pane: new(key: string) => NativeOwner,
 * selected?: string, select(key?: string): void, publish(view: View): void,
 * publishSurfaces(owners: (NativeOwner | ExternalOwner)[]): void,
 * renderSettings(view: View): void, renderResources(view: View): void}} context */
export function createViewProjector(context) {
  /** @param {View} next */
  function render(next) {
    context.rendererSlots = [];
    context.view = next;
    if (next.notice !== context.lastNotice) {
      context.lastNotice = next.notice;
      context.notify(next.notice);
    }
    for (const [key, pane] of context.panes)
      if (
        !Object.hasOwn(next.surfaces, key) ||
        (pane.kind === "external") !== (next.surfaces[key]?.kind === "external")
      ) {
        pane.dispose();
        context.panes.delete(key);
      }
    for (const [key, data] of Object.entries(next.surfaces)) {
      if (!context.panes.has(key))
        context.panes.set(
          key,
          data?.kind === "external"
            ? new context.ExternalPane(key)
            : new context.Pane(key),
        );
      const pane = context.panes.get(key);
      const submitKey = next.preferences?.submit_key ?? "enter",
        composerActions = next.composer_actions?.[key];
      if (pane?.kind === "external" && data?.kind === "external") {
        pane.staged = {data, submitKey};
      } else if (pane && pane.kind !== "external" && data?.kind !== "external") {
        pane.staged = {data, models: next.catalog.models, submitKey, composerActions};
      } else throw new Error("Surface owner does not match presentation kind");
    }
    if (!context.selected || !context.panes.has(context.selected))
      context.selected = context.panes.keys().next().value;
    context.select(context.selected);
    context.publish(next);
    context.publishSurfaces([...context.panes.values()]);
    context.renderSettings(next);
    context.renderResources(next);
  }

  return render;
}
