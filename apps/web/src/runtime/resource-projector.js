export function createResourceProjector(context) {
  let resourceCapture;
  function renderResources(next) {
    const scope = JSON.stringify(
      Object.entries(next.surfaces ?? {}).map(([key, value]) => [
        key,
        value?.generation,
      ]),
    );
    const unchanged =
      resourceCapture?.owner === context.connection &&
      resourceCapture.panels === next.panels &&
      resourceCapture.media === next.has_media &&
      resourceCapture.scope === scope;
    resourceCapture = {
      owner: context.connection,
      panels: next.panels,
      media: next.has_media,
      scope,
    };
    if (!unchanged)
      context.connection?.resources.observe(next.panels ?? [], next.surfaces);
    // Slots are collected afresh for every frame, even when its panel array is shared.
    const keep = new Set();
    for (const entry of next.panels ?? []) {
      keep.add(entry.view);
      let target = context.resourceHosts.get(entry.view);
      if (!target) {
        const body = context.element("div", "resource-content");
        context.$("resource-parking").append(body);
        target = {
          body,
          key: undefined,
          title: "Resource",
          entry,
          rendererKey: `resource-${entry.view}`,
          surface: "pane",
          show(key, title, content) {
            this.key = key;
            this.title = title;
            this.model = content;
          },
          clear() {
            this.key = undefined;
            this.model = undefined;
          },
        };
        context.resourceHosts.set(entry.view, target);
      }
      target.entry = entry;
      context.renderDetail({ ...entry, has_media: next.has_media }, target);
    }
    for (const [id, target] of context.resourceHosts)
      if (!keep.has(id)) {
        target.body.remove();
        context.resourceHosts.delete(id);
      }
    context.publishResources();
  }

  return renderResources;
}
