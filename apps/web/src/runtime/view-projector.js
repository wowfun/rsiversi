export function createViewProjector(context) {
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
      pane.staged = {
        data,
        models: next.catalog.models,
        submitKey,
        composerActions,
      };
    }
    if (!context.panes.has(context.selected))
      context.selected = context.panes.keys().next().value;
    context.select(context.selected);
    context.publish(next);
    context.publishSurfaces([...context.panes.values()]);
    context.renderSettings(next);
    context.renderResources(next);
  }

  return render;
}
