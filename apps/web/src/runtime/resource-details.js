// @ts-check
/** @typedef {import('../details/model.ts').ResourceContent} ResourceContent */
/** @typedef {import('../details/model.ts').Call} Call */
/** @typedef {import('../resource-hosts.ts').PanelEntry & {has_media:boolean}} DetailFrame */
/** @typedef {{key:string,surface:string,root:HTMLElement|null|undefined,binding:string,
 * snapshot:{revision:string,model:unknown,busy:boolean,error:string|null},
 * host:{invoke(action:string,input:unknown):Promise<unknown>,source(name:string,offset:number,maximum:number):Promise<unknown>}}} RendererSlot */
/** @typedef {{connection:object,boundCall(owner:object):Call,
 * acquireImage(owner:object,media:DetailFrame['source_media'],ticket:string,call:Call):import('../details/model.ts').ImageInterest,
 * rendererSlots:RendererSlot[]}} Context */
import {interactionKey} from './interaction.js';
/** @param {Context} context */
export function createResourceDetails(context) {
  /** @type {WeakMap<import('../resource-hosts.ts').ResourceHost,{inputs:unknown[],slot:RendererSlot|undefined}>} */
  const retained = new WeakMap();
  /** @param {DetailFrame} next @param {import('../resource-hosts.ts').ResourceHost} target @param {object} identity */
  function render(next, target, identity = next) {
    const inputs = [context.connection, identity, next.source_media, next.has_media];
    const previous = retained.get(target);
    if (previous && previous.inputs.every((input, index) => input === inputs[index])) {
      if (previous.slot) context.rendererSlots.push(previous.slot);
      return;
    }
    const before = context.rendererSlots.length;
    project(next, target);
    retained.set(target, {inputs, slot:context.rendererSlots[before]});
  }
  /** @param {DetailFrame} next @param {import('../resource-hosts.ts').ResourceHost} target */
  function project(next, target) {
    const connection = context.connection,
      call = context.boundCall(connection),
      /** @type {import('../details/model.ts').Command} */
      command = (value) => call("command", JSON.stringify(value));
    /** @param {string} key @param {string} title @param {ResourceContent} content */
    const show = (key, title, content) =>
      target.show(key, title, { ...content, command, call });
    if (next.image_detail) {
      const detail = next.image_detail;
      show(`image:${detail.ticket}`, "Image preview", {kind:"image",data:{
        ...detail,
        owner: connection,
        acquire: () =>
          context.acquireImage(connection, detail.media, detail.ticket, call),
      }});
      return;
    }
    if (next.ui_detail) {
      const detail = next.ui_detail, model = detail.model,
        binding =
          model && detail.binding
            ? JSON.stringify(detail.binding)
            : undefined;
      show(
        binding
          ? `ui:${binding}`
          : `ui-loading:${detail.ticket}:${detail.error ?? ""}`,
        detail.model?.standard_view?.title ?? "Card details",
        {kind:"ui",data:detail},
      );
      if (binding && model)
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
                !detail.model?.actions.some((item) => item.name === action)
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
        {kind:"extensions",data:next.remote_ui_catalog},
      );
      return;
    }
    if (next.block_sources) {
      show(
        `block-sources:${next.block_sources.ticket}`,
        "Block sources",
        {kind:"block-sources",data:next.block_sources},
      );
      return;
    }
    if (next.source_detail) {
      const detail = next.source_detail;
      show(
        JSON.stringify([detail, next.source_media]),
        "Exact source",
        {kind:"source",data:{
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
        }},
      );
      return;
    }
    if (next.detail) {
      const detail = next.detail,
        key = interactionKey(detail);
      show(
        key,
        detail.kind === "approval"
          ? detail.request.action
          : detail.request.review
            ? "Review plan"
            : "Answer the assistant",
        {kind:"interaction",data:detail},
      );
      return;
    }
    target.clear();
  }
  return render;
}
