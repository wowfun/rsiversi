import {conversationRecency} from "../navigation-presentation.js";
import {messageClock} from '../message-metrics.js';
import { IconArchiveOffOutlineRegular, IconArchiveOutlineRegular, IconCloseOutlineRegular, IconCopyOutlineRegular,
  IconEditOutlineRegular, IconMailReadOutlineRegular, IconMailUnreadOutlineRegular, IconEllipsisOutlineRegular, IconPinOutlineRegular } from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { useRef, useState, type SyntheticEvent } from "react";
import { Menu } from "../vendor/dsh/dockkit-primitives/Menu.tsx";
import { Modal } from "./presentation.tsx";
import { Button } from "./button.tsx";
import dialogCss from "./workspace-header.module.css";
import css from "./navigation.module.css";
import {isUnread, markReading} from "./session-reading.ts";
import { ActivityLabel } from "./activity-label.tsx";
import { rowContextMenu } from "./row-context-menu.ts";
import {
  input,
  run,
  type NavigationEntry,
  type AttentionEntry, type ArchiveActivity,
} from "./bridge.ts";
import type { useDeviceNavigation } from "./navigation-device.ts";
export const archivedSessionHint = "Unarchive this session to open it.";
export interface SessionMenuTarget {
  entry: NavigationEntry;
  opener: HTMLElement;
  point: {x: number; y: number};
  revision?: string;
}

export function ConversationRow({
  entry,
  active,
  title,
  activity,
  workspaceLabel,
  openSearchResult,
  unarchive,
  device,
  onMenu,
  opener,
  now,
}: {
  entry: NavigationEntry;
  active: boolean;
  title: string | undefined;
  activity: AttentionEntry | undefined;
  workspaceLabel?: string;
  openSearchResult?: (entry: NavigationEntry) => unknown;
  unarchive?: () => Promise<unknown>;
  device: ReturnType<typeof useDeviceNavigation>;
  now: number;
  onMenu(target?: SessionMenuTarget): void;
  opener?: HTMLElement;
}) {
  const row = useRef<HTMLDivElement>(null);
  const menuOpen = !!opener && !!row.current?.contains(opener);
  const clock = workspaceLabel ? undefined : messageClock(entry.last_activity_ms, now);
  function showMenu(event: SyntheticEvent<HTMLElement>, point?: SessionMenuTarget["point"]) {
    if (event.defaultPrevented) return;
    event.preventDefault();
    event.stopPropagation();
    const opener = event.target instanceof Element ? event.target.closest("button") : null;
    if (!opener) return;
    opener.focus({preventScroll: true});
    const rect = opener.getBoundingClientRect();
    onMenu({entry, opener, point: point ?? {x: rect.left, y: rect.bottom}});
  }
  const gestures = rowContextMenu(showMenu);
  return (
    <div
      ref={row}
      className="session-row"
      data-menu-open={menuOpen || undefined}
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
      {...gestures}
      onKeyDown={(event) => {
        gestures.onKeyDown(event);
        if (event.defaultPrevented) return;
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
        aria-description={openSearchResult && entry.metadata.archived ? archivedSessionHint : undefined}
        aria-haspopup="menu"
        title={`${entry.path}\n${entry.session}`}
        onClick={() =>
          run(() => openSearchResult ? openSearchResult(entry) : input.open({ action: "open", session: entry.session }))
        }
      >
        {workspaceLabel ? <span className={css.searchLabel}><strong>{title}</strong><small>{workspaceLabel}</small></span> : <strong>{title}</strong>}
        <ActivityLabel state={activity?.status} failed={!!activity?.reading.failed} iconOnly />
        {!workspaceLabel && <time className="session-recency" dateTime={clock?.iso} title={clock?.title}>{conversationRecency(entry.last_activity_ms, now)}</time>}
      </button>
      {unarchive ? <button type="button" className={css.sessionActions} aria-label="Unarchive session"
        title="Unarchive session" onClick={() => void run(unarchive)}>
        <IconArchiveOffOutlineRegular size={16}/>
      </button> : <button type="button" className={css.sessionActions} aria-label={`Session actions for ${title ?? entry.session}`}
        aria-haspopup="menu" aria-expanded={menuOpen} data-session-menu-trigger=""
        onClick={event => opener === event.currentTarget ? onMenu() : showMenu(event)}>
        <IconEllipsisOutlineRegular size={16}/>
      </button>}
    </div>
  );
}

export function SessionMenu({target, device, reading, epoch, close, copy, rename, mutate, archive}: {
  target: SessionMenuTarget;
  reading?: AttentionEntry;
  epoch?: string;
  device: ReturnType<typeof useDeviceNavigation>;
  close(): void;
  copy(): void;
  rename(): void;
  archive?: () => Promise<void>;
  mutate(metadata: NavigationEntry["metadata"]): Promise<unknown>;
}) {
  const {entry, opener, point} = target;
  const {metadata, session} = entry;
  const select = (id: string) => {
    close();
    switch (id) {
      case "mark":
        if (epoch) void run(() => markReading(epoch, session, reading, isUnread(reading) ? "read" : "unread"));
        break;
      case "rename":
        opener.focus({preventScroll: true});
        rename();
        break;
      case "pin": void run(() => mutate({...metadata, pinned: !metadata.pinned})); break;
      case "archive": if (archive) void run(archive); break;
      case "copy": copy(); break;
      case "first": case "last": void run(() => device.move(session, id)); break;
    }
  };
  return <Menu open anchor={null} portal={opener.closest('dialog') ?? true} autoFocus contextual returnFocus={() => opener} anchorPoint={point}
    isExternalTrigger={node => node instanceof Element && !!node.closest("[data-session-menu-trigger]")}
    items={[
      ...(metadata.archived ? [] : [{id: "pin", label: metadata.pinned ? "Unpin session" : "Pin session", icon: <IconPinOutlineRegular/>}]),
      ...(metadata.archived ? [] : [{id: "mark", label: isUnread(reading) ? "Mark as read" : "Mark as unread", disabled: !epoch,
        icon: isUnread(reading) ? <IconMailReadOutlineRegular/> : <IconMailUnreadOutlineRegular/>,
        shortcut: {keys: [/Mac/.test(navigator.platform) ? "⌘" : "Ctrl", "Shift", "U"]}}]),
      {id: "rename", label: "Rename", icon: <IconEditOutlineRegular/>},
      {id: "first", label: "Move to first", disabled: !device.ready},
      {id: "last", label: "Move to last", disabled: !device.ready},
      {type: "separator", id: "copy-separator"},
      {id: "copy", label: "Copy Session ID", icon: <IconCopyOutlineRegular/>},
    ]}
    footer={archive ? [{id: "archive", label: metadata.archived ? "Unarchive session" : "Archive session",
      icon: metadata.archived ? <IconArchiveOffOutlineRegular/> : <IconArchiveOutlineRegular/>}] : []}
    onClose={close} onSelect={select}/>;
}

export function ConfirmArchive({title, activity, close, confirm}: {
  title: string; activity: ArchiveActivity[]; close(): void; confirm(): Promise<void>;
}) {
  const [busy, setBusy] = useState(false), [error, setError] = useState("");
  const dismiss = () => { if (!busy) close(); };
  return <Modal label="Stop and archive this session?" className={dialogCss.dialog} close={dismiss} initialFocus="[data-archive-cancel]">
    <header><h2>Stop and archive this session?</h2><button type="button" onClick={dismiss} disabled={busy} aria-label="Close"><IconCloseOutlineRegular/></button></header>
    <p>“{title}” still has work in progress. You can restore the session later from the sidebar’s All conversations filter. Stopped work will not resume on its own.</p>
    <ul className={dialogCss.archiveActivity} aria-label="Work that will be stopped">{activity.map(entry => <li key={entry.kind}>
      {entry.kind === "turn" ? "The turn in progress" : `${entry.items.length} ${entry.kind === "job" ? "background job" : "running subagent"}${entry.items.length === 1 ? "" : "s"}: ${entry.items.map(item => item.label || item.id).join(", ")}`}
    </li>)}</ul>
    {error && <p role="alert" className={dialogCss.danger}>{error}</p>}
    <footer><Button data-archive-cancel variant="outline" disabled={busy} onClick={dismiss}>Cancel</Button>
      <Button variant="outline" className={dialogCss.danger} disabled={busy} onClick={async () => {
        setBusy(true);
        try { await confirm(); close(); } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); setBusy(false); }
      }}>{busy ? "Stopping and archiving…" : "Stop and archive"}</Button></footer>
  </Modal>;
}

export function RenameSession({title, close, save}: {title: string; close(): void; save(value: string): Promise<unknown>}) {
  const [value, setValue] = useState(title), [busy, setBusy] = useState(false), [error, setError] = useState("");
  const composing = useRef(false);
  const dismiss = () => { if (!busy) close(); };
  return <Modal label="Rename session" className={dialogCss.dialog} close={dismiss} initialFocus="input">
    <form aria-busy={busy} onSubmit={async event => {
      event.preventDefault();
      if (busy) return;
      setBusy(true);
      try { await save(value.trim()); close(); }
      catch (cause) { setBusy(false); setError(cause instanceof Error ? cause.message : String(cause)); }
    }}>
      <header><h2>Rename session</h2><button type="button" aria-label="Close rename session" onClick={dismiss} disabled={busy}><IconCloseOutlineRegular/></button></header>
      <input aria-label="Session name" maxLength={256} value={value} disabled={busy}
        onFocus={event => event.target.select()} onChange={event => {setValue(event.target.value); setError("");}}
        onCompositionStart={() => {composing.current = true;}} onCompositionEnd={() => {composing.current = false;}}
        onKeyDown={event => {if (event.key === "Enter" && (event.nativeEvent.isComposing || composing.current)) event.preventDefault();}}/>
      {error && <p role="alert" className={dialogCss.danger}>{error}</p>}
      <footer><Button variant="outline" disabled={busy} onClick={dismiss}>Cancel</Button><Button variant="primary" type="submit" disabled={busy}>Rename</Button></footer>
    </form>
  </Modal>;
}
