import { Fragment } from "react";
import type {
  ProjectionActions,
  SessionProjections,
  Register,
} from "./projection-types.ts";
import { run } from "../bridge.ts";

export function PendingInputs({
  data,
  owner,
  register,
}: {
  data?: SessionProjections | null;
  owner: ProjectionActions;
  register: Register;
}) {
  const items = data?.protected ? [] : (data?.queue ?? []);
  return (
    <section
      ref={register}
      className="queue-list"
      aria-label="Pending inputs"
      hidden={!items.length}
    >
      {!!items.length && (
        <strong className="queue-heading">
          Pending inputs · {items.length}
        </strong>
      )}
      {items.map((item) => (
        <div
          key={item.queue_slot.id}
          className="queue-row"
          data-slot={item.queue_slot.id}
        >
          <span className="queue-summary">
            {item.source_kind} · {item.delivery_label} · {item.message_id}
          </span>
          {[
            [item.read_label, "read"],
            [item.withdraw_label, "withdraw"],
            [item.convert_label, "convert_to_steer"],
          ].map(
            ([label, action]) =>
              label && (
                <button
                  key={action}
                  type="button"
                  className="quiet queue-action"
                  disabled={!owner.queueBinding}
                  onClick={() =>
                    void run(() =>
                      owner.queueAction(item.queue_slot.id, action!),
                    )
                  }
                >
                  {label}
                </button>
              ),
          )}
        </div>
      ))}
    </section>
  );
}

export function PendingInteractions({
  data,
  owner,
}: {
  data?: SessionProjections | null;
  owner: ProjectionActions;
}) {
  return (
    <div className="pending">
      {!data?.protected &&
        data?.pending?.map((item) => (
          <button
            key={`${item.owner}:${item.id}`}
            type="button"
            data-interaction-owner={item.owner}
            data-interaction-id={item.id}
            onClick={() =>
              void run(() =>
                owner.action("inspect_interaction", {
                  owner: item.owner,
                  id: item.id,
                }),
              )
            }
          >
            {item.kind === "approval" ? "Review" : "Answer"}: {item.title}
          </button>
        ))}
    </div>
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
    result = receipt?.outcome;
  return (
    <div
      className="session-commands"
      hidden={(!owner.commandsExpanded || !data?.commands?.commands.length) && !receipt}
    >
      {owner.commandsExpanded && data?.commands?.commands.map((item) => (
        <Fragment key={item.name}>
          <button
            type="button"
            className="quiet"
            title={item.description}
            disabled={
              data.commands?.revision.kind === "draft" && !item.draft_safe
            }
            onClick={() => void run(() => owner.selectCommand(item.name))}
          >
            /{item.name}
          </button>
          <span className="command-description">{item.description}</span>
        </Fragment>
      ))}
      {receipt && result && (
        <p className="command-receipt">
          {receipt.command} ·{" "}
          {result.kind === "draft_changed"
            ? `Draft changed · revision ${result.revision}`
            : `Committed · control ${result.control_seq}`}{" "}
          · {receipt.request_id}
        </p>
      )}
    </div>
  );
}
