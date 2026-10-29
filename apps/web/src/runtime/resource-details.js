import {interactionKey} from './interaction.js';
export function createResourceDetails(context) {
  return function render(next, target) {
    const connection = context.connection,
      call = context.boundCall(connection),
      command = (value) => call("command", JSON.stringify(value));
    const show = (key, title, kind, data) =>
      target.show(key, title, { kind, data, command, call });
    if (next.image_detail) {
      const detail = next.image_detail;
      show(`image:${detail.ticket}`, "Image preview", "image", {
        ...detail,
        owner: connection,
        acquire: () =>
          context.acquireImage(connection, detail.media, detail.ticket, call),
      });
      return;
    }
    if (next.ui_detail) {
      const detail = next.ui_detail,
        binding =
          detail.model && detail.binding
            ? JSON.stringify(detail.binding)
            : undefined;
      show(
        binding
          ? `ui:${binding}`
          : `ui-loading:${detail.ticket}:${detail.error ?? ""}`,
        detail.model?.standard_view?.title ?? "Card details",
        "ui",
        detail,
      );
      if (binding)
        context.rendererSlots.push({
          key: target.rendererKey,
          surface: target.surface,
          get root() {
            return target.rendererRoot;
          },
          binding,
          snapshot: {
            revision: detail.ticket,
            model: detail.model,
            busy: detail.busy,
            error: detail.error,
          },
          host: {
            invoke(action, input) {
              if (
                detail.busy ||
                !detail.model.actions.some((item) => item.name === action)
              )
                throw Error("This action is no longer available");
              return command({
                action: "ui_invoke",
                ticket: detail.ticket,
                name: action,
                input,
              });
            },
            source(name, offset, maximum) {
              return call(
                "ui_source",
                JSON.stringify({
                  ticket: detail.ticket,
                  name,
                  offset,
                  maximum,
                }),
              );
            },
          },
        });
      return;
    }
    if (next.remote_ui_catalog) {
      show(
        JSON.stringify(next.remote_ui_catalog),
        "Service extensions",
        "extensions",
        next.remote_ui_catalog,
      );
      return;
    }
    if (next.block_sources) {
      show(
        `block-sources:${next.block_sources.ticket}`,
        "Block sources",
        "block-sources",
        next.block_sources,
      );
      return;
    }
    if (next.source_detail) {
      const detail = next.source_detail;
      show(
        JSON.stringify([detail, next.source_media]),
        "Exact source",
        "source",
        {
          ...detail,
          owner: connection,
          media: next.has_media ? next.source_media : undefined,
          acquire: () =>
            context.acquireImage(
              connection,
              next.source_media,
              detail.ticket,
              call,
            ),
        },
      );
      return;
    }
    if (next.detail) {
      const detail = next.detail,
        request = detail.request,
        key = interactionKey(detail);
      show(
        key,
        detail.kind === "approval"
          ? request.action
          : request.review
            ? "Review plan"
            : "Answer the assistant",
        "interaction",
        detail,
      );
      return;
    }
    target.clear();
  };
}
