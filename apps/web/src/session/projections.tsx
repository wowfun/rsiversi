import { Fragment, useEffect, useState, useSyncExternalStore } from "react";
import type { ProjectionActions, SessionProjections } from "./projection-types.ts";
import { run } from "../bridge.ts";
import type { NativeSurface } from "../presentation-types.ts";
import { composerInteraction, resources, type ResourceHost } from "../resource-hosts.ts";
import resourceCss from "../resources.module.css";
import css from "./surface.module.css";
import { InteractionDetail } from "../details/interaction.tsx";
import type { InteractionData, ObservedRequest } from "../details/model.ts";
import {pruneQuestionProgress, useQuestionScope} from "../details/question-progress.ts";

function ObservedInteraction({observed, requestOwner, owner, generation, session}: {
  observed: ObservedRequest;
  requestOwner: string;
  owner: ProjectionActions & {index: string};
  generation: string;
  session: string;
}) {
  const [checked, setChecked] = useState<{
    observed: ObservedRequest;
    value: ObservedRequest | null;
    revision: number;
  }>();
  const current = checked?.observed === observed ? checked.value : observed;
  if (!current) return <p role="status">This request is no longer pending.</p>;
  const data = {...current, pane: owner.index, generation, ticket: String(checked?.revision ?? 0)};
  async function command({action, ...fields}: Record<string, unknown>) {
    if (action === "inspect_interaction") {
      const value = await owner.action("read_pending_interaction", {
        kind: observed.kind, owner: requestOwner, id: observed.request.id,
      }) as ObservedRequest | null;
      setChecked(previous => ({observed, value, revision: (previous?.revision ?? 0) + 1}));
    } else {
      await owner.action(String(action), fields);
    }
  }
  return (
    <div className={`${css.approval} ${resourceCss.scope}`}
      data-interaction-owner={requestOwner} data-interaction-id={observed.request.id}>
      <InteractionDetail data={data} command={command} session={session} />
    </div>
  );
}

function matchesInteraction(detail: InteractionData | null, item?: {kind:string; owner:string; id:string}) {
  if (!detail || !item || detail.kind !== item.kind || detail.request.id !== item.id) return false;
  const owner = detail.kind === "approval" ? detail.request.subject.session_id : detail.request.session_id;
  return owner === item.owner;
}

const noPending: never[] = [];

export function PendingInteractions({data, owner}: {
  data?: NativeSurface | null;
  owner: ProjectionActions & {index: string};
}) {
  const hosts = useSyncExternalStore(resources.subscribe, resources.getSnapshot);
  const scope = useQuestionScope(data?.protected ? undefined : data?.session);
  const pending = data?.pending?.length === 0 ? noPending : data?.pending;
  useEffect(() => {
    if (scope && pending) pruneQuestionProgress(scope, pending);
  }, [scope, pending]);
  if (!data || data.protected) return null;
  const selected = data.pending?.find(item => item.review);
  let explicit: ResourceHost | undefined;
  for (const host of hosts) {
    const entry = host.entry;
    if (composerInteraction(host) && entry.pane === owner.index && entry.session === data.session &&
        entry.generation === data.generation && host.model?.kind === 'interaction' &&
        (!explicit || BigInt(entry.open_revision) > BigInt(explicit.entry.open_revision))) explicit = host;
  }
  const chosenDetail = explicit?.entry.detail ?? null;
  const showObserved = !chosenDetail || matchesInteraction(chosenDetail, selected);
  const buttons = data.pending?.filter(item => showObserved ? item !== selected : !matchesInteraction(chosenDetail, item));
  return (
    <>
      <div className="pending">
        {buttons?.map(item => (
          <button key={JSON.stringify([item.kind, item.owner, item.id])} type="button"
            data-interaction-owner={item.owner} data-interaction-id={item.id}
            onClick={() => void run(() => owner.action("inspect_interaction", {owner:item.owner, id:item.id}))}>
            {item.kind === "approval" ? "Review" : "Answer"}: {item.title}
          </button>
        ))}
      </div>
      {showObserved && selected?.review && <ObservedInteraction
        key={JSON.stringify([data.session, data.generation, selected.kind, selected.owner, selected.id])}
        observed={selected.review} requestOwner={selected.owner} owner={owner} generation={data.generation} session={data.session} />}
      {!showObserved && explicit?.model?.kind === 'interaction' &&
        <div className={`${css.approval} ${resourceCss.scope}`}>
          <InteractionDetail key={explicit.key} data={explicit.model.data} command={explicit.model.command} session={data.session} />
        </div>}
    </>
  );
}

export function SessionCommands({
  data,
  owner,
}: {
  data?: SessionProjections | null;
  owner: ProjectionActions;
}) {
  const receipt = data?.command_receipt,
    result = receipt?.outcome,
    commands = owner.commandsExpanded
      ? data?.commands?.commands.filter((item) => item.composer_input)
      : undefined;
  return (
    <div
      className="session-commands"
      hidden={!commands?.length && !receipt}
    >
      {commands?.map((item) => (
        <Fragment key={item.name}>
          <button
            type="button"
            className="quiet"
            title={item.description}
            disabled={
              data?.commands?.revision.kind === "draft" && !item.draft_safe
            }
            onClick={() => void run(() => owner.selectCommand(item))}
          >
            /{item.name}
          </button>
          <span className="command-description">{item.description}</span>
        </Fragment>
      ))}
      {receipt && result && (
        <details className="command-receipt">
          <summary>{receipt.command} · {result.kind === "draft_changed" ? "Draft changed" : "Committed"}</summary>
          {result.kind === "draft_changed"
            ? `Revision ${result.revision}`
            : `Control ${result.control_seq}`}{" "}
          · Request {receipt.request_id}
        </details>
      )}
    </div>
  );
}
