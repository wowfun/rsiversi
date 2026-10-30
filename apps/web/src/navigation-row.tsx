import {conversationRecency} from "../navigation-presentation.js";
import {messageClock} from '../message-metrics.js';
import { IconEllipsisOutlineRegular } from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { Button } from "./button.tsx";
import { ActivityLabel } from "./activity-label.tsx";
import {
  input,
  useSelected,
  useView,
  run,
  type NavigationEntry,
  type AttentionEntry,
} from "./bridge.ts";
import type { useDeviceNavigation } from "./navigation-device.ts";
export function ConversationRow({
  entry,
  title,
  activity,
  device,
  onRename,
  mutate,
  now,
}: {
  entry: NavigationEntry;
  title: string | undefined;
  activity: AttentionEntry["status"] | undefined;
  device: ReturnType<typeof useDeviceNavigation>;
  now: number;
  onRename(entry: NavigationEntry): void;
  mutate(
    session: string,
    metadata: NavigationEntry["metadata"],
  ): Promise<unknown>;
}) {
  const selected=useSelected(value=>value);
  const active=useView(value=>value?.surfaces[selected]?.session===entry.session);
  const clock = messageClock(entry.last_activity_ms, now);
  return (
    <div
      className="session-row"
      data-testid="conversation-row"
      data-session-id={entry.session}
      key={entry.session}
      draggable={device.ready}
      onDragStart={(event) =>
        event.dataTransfer.setData("application/rsi-session", entry.session)
      }
      onDragOver={(event) => {
        if (event.dataTransfer.types.includes("application/rsi-session"))
          event.preventDefault();
      }}
      onDrop={(event) => {
        event.preventDefault();
        const id = event.dataTransfer.getData("application/rsi-session");
        if (id) void run(() => device.move(id, "before", entry.session));
      }}
      onKeyDown={(event) => {
        if (
          event.altKey &&
          ["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)
        ) {
          event.preventDefault();
          const positions = {
            ArrowUp: "previous",
            ArrowDown: "next",
            Home: "first",
            End: "last",
          } as const;
          void run(() =>
            device.move(
              entry.session,
              positions[event.key as keyof typeof positions],
            ),
          );
        }
      }}
    >
      <button
        className="nav-item"
        data-testid="conversation-open"
        aria-current={active||undefined}
        title={`${entry.path}\n${entry.session}`}
        onClick={() =>
          run(() => input.open({ action: "open", session: entry.session }))
        }
      >
        <strong>{title}</strong>
        <ActivityLabel state={activity} iconOnly />
        <time className="session-recency" dateTime={clock?.iso} title={clock?.title}>{conversationRecency(entry.last_activity_ms, now)}</time>
      </button>
      <details className="session-options">
        <summary aria-label={`Options for ${title}`}>
          <IconEllipsisOutlineRegular />
        </summary>
        <div className="session-menu-items">
        <Button
          size="sm"
          onClick={() =>
            run(() =>
              mutate(entry.session, {
                ...entry.metadata,
                pinned: !entry.metadata.pinned,
              }),
            )
          }
          disabled={entry.metadata.archived}
        >
          {entry.metadata.pinned ? "Unpin" : "Pin"}
        </Button>
        <Button
          size="sm"
          onClick={() => {
            onRename(entry);
          }}
        >
          Rename
        </Button>
        <Button
          size="sm"
          onClick={() =>
            run(() =>
              mutate(entry.session, {
                ...entry.metadata,
                pinned: false,
                archived: !entry.metadata.archived,
              }),
            )
          }
        >
          {entry.metadata.archived ? "Unarchive" : "Archive"}
        </Button>
        <Button
          size="sm"
          disabled={!device.ready}
          onClick={() => run(() => device.move(entry.session, "first"))}
        >
          Move to first
        </Button>
        <Button
          size="sm"
          disabled={!device.ready}
          onClick={() => run(() => device.move(entry.session, "last"))}
        >
          Move to last
        </Button>
        <p className="hint">
          {entry.path}
          <br />
          {entry.session}
        </p>
        </div>
      </details>
    </div>
  );
}
