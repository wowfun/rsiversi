import { archivedSessionHint, BlankConversationRow, ConfirmArchive, ConversationRow, RenameSession, SessionMenu, type SessionMenuTarget } from "./navigation-row.tsx";
import { WorkspaceNavigation } from "./navigation-workspaces.tsx";
import {NavigationOptions, type ConversationFilter} from "./navigation-options.tsx";
import { AttentionNavigation } from "./navigation-attention.tsx";
import { ExternalNavigation } from "./navigation-external.tsx";
import {Toast} from "../vendor/dsh/dockkit-primitives/Toast.tsx";
import {Tooltip} from "../vendor/dsh/dockkit-primitives/Tooltip.tsx";
import {writeClipboard} from "../mounts.js";
import {
  IconSearchOutlineRegular,
  IconProjectAddOutlineRegular,
  IconCloseOutlineRegular,
  IconWarningOutlineRegular,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import css from "./navigation.module.css";
import { presentationIdentity, useLayout } from "./presentation.tsx";
import { recentFirst, useDeviceNavigation } from "./navigation-device.ts";
import { TextSearchIndex } from "../vendor/dsh/text-search.ts";
import {workspaceName} from "../workspace-name.js";
import { useEffect, useMemo, useRef, useState } from "react";
import { chooseWorkspace } from "./directory-picker.tsx";
import { Button } from "./button.tsx";
import { input, run, source, selection, useView, useSelected, type ArchiveActivity, type NavigationEntry } from "./bridge.ts";
import {
  navigationTitles, navigationAttention, sessionWorkspaceGroup, sidebarConversations,
  GroupRestoration,
} from "../navigation-presentation.js";

function searchInput(value: string) {
  return value.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").slice(0, 500).replace(/[\uD800-\uDBFF]$/, "");
}

export interface NavigationProps {
  conversationVisible: boolean;
  searchOpen: boolean;
  setSearchOpen(open: boolean): void;
  searchInputRef: React.RefObject<HTMLInputElement>;
}

const mac = /Mac/.test(navigator.platform);
const searchKeys = mac ? ["⌥","⌘","K"] : ["Ctrl","+","Alt","+","K"];
export function NavigationSearchButton({rail=false, open}: {rail?:boolean; open:()=>void}) {
  return <Tooltip label="Search conversations" shortcutKeys={searchKeys} side={rail ? "right" : "bottom"} delayMs={rail ? 0 : 500} portal>
    <Button size={rail ? "md" : "sm"} aria-label="Search conversations" aria-keyshortcuts={mac ? "Alt+Meta+K" : "Control+Alt+K"} onClick={open}>
      <IconSearchOutlineRegular size={rail ? 18 : 16}/>
    </Button>
  </Tooltip>;
}

export function Navigation({searchOpen, setSearchOpen, searchInputRef, conversationVisible}: NavigationProps) {
  const selected = useSelected(value=>value);
  const currentSession = useView(view=>{
    const surface = view?.surfaces[selected];
    return surface?.kind === "native" ? surface.session : undefined;
  });
  const [now, setNow] = useState(Date.now);
  useEffect(()=>{const timer=setInterval(()=>setNow(Date.now()), 60_000);return ()=>clearInterval(timer);}, []);
  const nav = useView((value) => value?.navigation),
    catalog = useView((value) => value?.catalog);
  const [mode, setMode] = useState<ConversationFilter>("active");
  const archived = mode === "all" ? null : mode === "archived";
  const completeList = nav?.ticket && nav.continued === false && !nav.more &&
    !nav.newer_activity && !nav.diagnostic && !nav.filter.query &&
    nav.filter.workspace.kind === "all" && nav.filter.archived === archived;
  const selectedWorkspaces = useView((value) =>
    JSON.stringify(
      [
        ...new Set(
          Object.values(value?.surfaces ?? {})
            .map(surface => sessionWorkspaceGroup(surface, value?.navigation))
            .filter(Boolean),
        ),
      ].sort(),
    ),
  );
  const { layout, expand } = useLayout();
  const workspaceSeed = catalog?.workspace_order_seed;
  const completeWorkspaces = workspaceSeed?.kind === "available";
  const workspaces = useMemo(
    () =>
      (workspaceSeed?.kind === "available"
        ? workspaceSeed.records
        : (catalog?.workspaces ?? [])
      ).map((item) => ({ ...item, ...item.coordinates })),
    [workspaceSeed, catalog?.workspaces],
  );
  const device = useDeviceNavigation(nav, workspaces, !completeWorkspaces, currentSession);
  const restoring = useRef(new GroupRestoration());
  const selectedGroups = useMemo(() => JSON.parse(selectedWorkspaces) as string[], [selectedWorkspaces]);
  const selectedPin = nav?.pins.find(pin => pin.status === "available" && pin.entry.blank && pin.entry.session === currentSession);
  const blankPin = selectedPin?.status === "available" ? selectedPin.entry : undefined;
  const flatEntries = useMemo(() => {
    const entries = nav?.entries ?? [];
    return sidebarConversations(blankPin ? [blankPin, ...entries] : entries, currentSession);
  }, [nav?.entries, currentSession, blankPin]);
  const currentGroup = useView(value => sessionWorkspaceGroup(value?.surfaces[selected], value?.navigation));
  const showUngrouped = currentGroup === "other" || !!nav &&
    (!completeList || flatEntries.some((entry: NavigationEntry) => entry.workspace === null));
  const firstVisits = useMemo(() => selectedGroups.filter(id =>
    (id === "other" || workspaces.some(record => record.id === id)) && !layout.workspaces.some(item => item.id === id)),
    [selectedGroups, workspaces, layout.workspaces]);
  useEffect(() => { for (const id of firstVisits) expand(id); }, [firstVisits, expand]);
  const restoreKey = useMemo(() => {
    const registered = new Set(workspaces.map(row => row.id));
    const desired = new Set(layout.workspaces.filter(item => item.expanded).map(item => item.id));
    for (const id of firstVisits) desired.add(id);
    return JSON.stringify(
      [...desired]
        .filter(
          (id) => !nav?.groups[id] && (id === "other" ? showUngrouped : registered.has(id)),
        )
        .sort(),
    );
  }, [workspaces, layout.workspaces, firstVisits, nav?.groups, showUngrouped]);
  useEffect(() => {
    if (!nav?.ticket || nav.filter.query) return;
    for (const id of JSON.parse(restoreKey) as string[]) {
      const workspace =
        id === "other" ? { kind: "unregistered" } : { kind: "registered", id };
      void run(() =>
        restoring.current.run(id, () =>
          device.command({
            action: "navigate",
            command: { kind: "group", workspace, ticket: null },
          }),
        ),
      );
    }
  }, [restoreKey, nav?.ticket, nav?.filter.query]);
  const [searchText, setSearchText] = useState("");
  const [reveal, setReveal] = useState<string>();
  const query = searchText.trim();
  const searchRoot = useRef<HTMLFormElement>(null);
  const submitSearch = useRef<() => void>();
  const dispatchedQuery = useRef<string | undefined>("");
  const [menu, setMenu] = useState<SessionMenuTarget>();
  const [edit, setEdit] = useState<SessionMenuTarget>();
  const [confirmation, setConfirmation] = useState<{title:string; activity:ArchiveActivity[]; confirm():Promise<void>}>();
  const [feedback, setFeedback] = useState<{sequence: number; text: string; success: boolean; undo?:()=>Promise<void>}>();
  const notify = (text: string, success = false, undo?:()=>Promise<void>) =>
    setFeedback(previous => ({sequence: (previous?.sequence ?? 0) + 1, text, success, undo}));
  const copySessionId = async (session: string) => {
    try {
      await writeClipboard(session);
      notify("Session ID copied", true);
    } catch {
      notify("Copy failed. Try again.");
    }
  };
  const filter = {
    query,
    archived,
    workspace: { kind: "all" },
  };
  const remoteQuery = completeList ? "" : query;
  const search = (value = filter, current?: () => boolean) =>
    device.command({
      action: "navigate",
      command: { kind: "query", filter: value },
    }, current);
  useEffect(() => {
    let active = true, queued = false;
    const dispatch = () => {
      if (queued || dispatchedQuery.current === remoteQuery) return;
      queued = true;
      void run(async () => {
        try {
          await search({...filter, query:remoteQuery}, () => {
            if (active) dispatchedQuery.current = remoteQuery;
            return active;
          });
        } catch (error) {
          if (active) {
            dispatchedQuery.current = undefined;
            throw error;
          }
        } finally { queued = false; }
      });
    };
    submitSearch.current = dispatch;
    const timer = remoteQuery ? window.setTimeout(dispatch, 250) : undefined;
    if (!remoteQuery) dispatch();
    return () => {
      active = false;
      clearTimeout(timer);
      submitSearch.current = undefined;
    };
  }, [remoteQuery, filter.archived]);
  useEffect(() => {
    if (!searchOpen || query) return;
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !searchRoot.current?.contains(event.target)) {
        setSearchOpen(false);
        setSearchText("");
      }
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [searchOpen, query]);
  const closeSearch = () => { setSearchOpen(false); setSearchText(""); };
  const mutate = (session: string, metadata: NavigationEntry["metadata"], revision = nav?.metadata_revision) =>
    device.command({
      action: "navigate",
      command: { kind: "replace", ticket: nav?.ticket, session, metadata, metadata_revision: revision },
    });
  const selectFilter = async (value: ConversationFilter) => {
    if (value !== "attention") await search({...filter, archived:value === "all" ? null : value === "archived"});
    setMode(value);
  };
  const archive = async (entry: NavigationEntry) => {
    const identity = presentationIdentity.getSnapshot(), pane = selection.getSnapshot();
    const current = () => {
      const surface = source.getSnapshot()?.surfaces[pane];
      return surface?.kind === "native" && surface.session === entry.session ? surface.generation : undefined;
    };
    const generation = current(), archived = !entry.metadata.archived;
    const restore = async () => {
      if (identity === presentationIdentity.getSnapshot()) await device.archive(entry.session, "restore");
    };
    if (!archived) return restore();
    const apply = async (stop: boolean) => {
      if (identity !== presentationIdentity.getSnapshot()) throw new Error("Connection changed before archive.");
      const activity = await device.archive(entry.session, stop ? 'stop_and_archive' : 'archive');
      if (identity !== presentationIdentity.getSnapshot()) return;
      if (activity) {
        setConfirmation({title: entry.metadata.title || titles.get(entry.session) || entry.session,
          activity, confirm:() => apply(true)});
      } else {
        notify(stop ? "Session stopped and archived. You can " : "Session archived. You can ", true, restore);
        if (generation && pane === selection.getSnapshot() && generation === current()) await input.open({action:"new_conversation"});
      }
    };
    await apply(false);
  };
  const visibleEntries = useMemo(() => [
    ...(nav?.entries ?? []),
    ...Object.values(nav?.groups ?? {}).flatMap((group) => group.entries),
    ...(nav?.pins ?? []).flatMap((pin) => pin.status === "available" ? [pin.entry] : []),
    ...(nav?.order?.entries ?? []).flatMap((entry) => entry ? [entry] : []),
  ], [nav?.entries, nav?.groups, nav?.pins, nav?.order?.entries]);
  const titles = useMemo(() => navigationTitles(visibleEntries), [visibleEntries]);
  const attention = useMemo(() => navigationAttention(nav?.attention), [nav?.attention]);
  const awaitingQuery = !!nav && nav.filter.query !== remoteQuery;
  const searching = query.length > 0;
  const workspaceLabels = useMemo(() => searching
    ? new Map(workspaces.map(item => [item.id, workspaceName(item.path, item.name)]))
    : undefined, [searching, workspaces]);
  const searchIndex = useMemo(() => {
    if (!workspaceLabels) return;
    const candidates = new Map<string, NavigationEntry>();
    for (const entry of visibleEntries) {
      if (!entry.blank && (filter.archived === null || entry.metadata.archived === filter.archived)) candidates.set(entry.session, entry);
    }
    return new TextSearchIndex([...candidates.values()].sort(recentFirst), entry => ({
      fuzzy: [titles.get(entry.session), entry.workspace ? workspaceLabels.get(entry.workspace) || workspaceName(entry.path) : "Ungrouped"],
      substring: [entry.session, entry.path],
    }));
  }, [workspaceLabels, visibleEntries, filter.archived, titles]);
  const searchEntries = useMemo(() => searchIndex?.search(query) ?? [], [searchIndex, query]);
  const openSearchResult = (entry: NavigationEntry) => {
    if (entry.metadata.archived) {
      notify(archivedSessionHint);
      return;
    }
    setReveal(entry.session);
    closeSearch();
    if (device.preferences.view !== "flat") expand(entry.workspace ?? "other");
    return input.open({action:"open", session:entry.session});
  };
  const row = (entry: NavigationEntry) => entry.blank
    ? <BlankConversationRow key={entry.session} session={entry.session} active={conversationVisible && currentSession === entry.session}/> : (
    <ConversationRow
      now={now}
      key={entry.session}
      entry={entry}
      active={conversationVisible && currentSession===entry.session}
      title={titles.get(entry.session)}
      activity={attention.get(entry.session)}
      workspaceLabel={workspaceLabels && (entry.workspace ? workspaceLabels.get(entry.workspace) || workspaceName(entry.path) : "Ungrouped")}
      openSearchResult={searching ? openSearchResult : undefined}
      onReveal={!searching && reveal === entry.session ? () => setReveal(id => id === entry.session ? undefined : id) : undefined}
      unarchive={searching && nav?.can_archive && entry.metadata.archived ? () => archive(entry) : undefined}
      device={device}
      opener={menu?.opener}
      onMenu={target => setMenu(target && {...target, revision: nav?.metadata_revision})}
    />
  );
  const pins = nav?.pins.filter(pin => pin.status === "missing" || !pin.entry.blank) ?? [];
  if (device.preferences.sessionOrder === "manual" && !device.paused) {
    const rank = new Map(device.ordered.map((id, index) => [id, index]));
    pins.sort(
      (a, b) =>
        (rank.get(a.status === "available" ? a.entry.session : a.session) ??
          Number.MAX_SAFE_INTEGER) -
        (rank.get(b.status === "available" ? b.entry.session : b.session) ??
          Number.MAX_SAFE_INTEGER),
    );
  }
  return (
    <>
      <div className={`section-heading ${css.heading}`}>
        {searchOpen ? <form ref={searchRoot} className="navigation-search" onSubmit={event=>{event.preventDefault();submitSearch.current?.()}}>
          <Button size="sm" type="submit" aria-label="Search"><IconSearchOutlineRegular size={14}/></Button>
          <input ref={searchInputRef} autoFocus data-navigation-initial-focus="" aria-label="Search conversations" placeholder="Search conversations" value={searchText} onChange={event=>{setReveal(undefined);setSearchText(searchInput(event.target.value));}} onKeyDown={event=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();closeSearch()}}}/>
          <Button size="sm" aria-label="Close search" onClick={closeSearch}><IconCloseOutlineRegular size={16}/></Button>
        </form> : <>
          <h2 data-testid="workspace-heading">Workspaces</h2>
          <div className="navigation-actions">
            <NavigationSearchButton open={()=>setSearchOpen(true)}/>
            <NavigationOptions device={device} filter={mode} onFilter={selectFilter} refresh={async()=>{await input.command({action:'refresh'});await search()}}/>
            <Button size="sm" data-testid="choose-workspace" aria-label="Choose workspace" onClick={chooseWorkspace}><IconProjectAddOutlineRegular size={16}/></Button>
          </div>
        </>}
      </div>
      {workspaceSeed?.kind === "too_large" && (
        <p role="status" className="hint">
          Workspace ordering is paused because complete membership exceeds its
          limit. Saved order is preserved.
        </p>
      )}
      {device.notice && (
        <p role="alert" className="settings-error">
          {device.notice}
        </p>
      )}
      {device.paused && (
        <p role="status" className="hint">
          Manual order is paused because complete membership exceeds its limit.
          Your saved order is preserved; showing updated conversations.
        </p>
      )}
      {nav?.newer_activity && (
        <p role="status" className="hint">
          Newer activity is available.{" "}
          <button onClick={() => run(() => search())}>
            Refresh conversations
          </button>
        </p>
      )}
      {nav?.diagnostic && (
        <p role="alert" className="settings-error">
          {nav.diagnostic}
        </p>
      )}
      {mode === "attention" && !query ? (
        <AttentionNavigation nav={nav} />
      ) : (
        <>
          {!query && !awaitingQuery && !!pins.length && (
            <section
              className="pinned-conversations"
              aria-label="Pinned conversations"
            >
              <h3>Pinned</h3>
              {pins.map((pin) =>
                pin.status === "available" ? (
                  row(pin.entry)
                ) : (
                  <div className="session-row stale" key={pin.session}>
                    <span>
                      {pin.metadata.title ?? pin.session} — unavailable
                    </span>
                    <Button
                      size="sm"
                      onClick={() =>
                        run(() =>
                          mutate(pin.session, {
                            ...pin.metadata,
                            pinned: false,
                          }),
                        )
                      }
                    >
                      Unpin
                    </Button>
                  </div>
                ),
              )}
            </section>
          )}
          <div id="sessions" className="nav-list">
            {awaitingQuery && !query ? <p role="status" className="hint">Loading conversations…</p> : !query && device.manual ? (
              <>
                {device.loading ? (
                  <p role="status" className="hint">
                    Loading conversations…
                  </p>
                ) : device.preferences.view === "flat" ? (
                  device.entries.map(row)
                ) : (
                  <WorkspaceNavigation conversationVisible={conversationVisible} nav={nav} device={device} workspaces={workspaces} completeWorkspaces={completeWorkspaces} showUngrouped={showUngrouped} expand={expand} row={row} attention={attention} blankPin={blankPin}/>
                )}
                <div className="manual-pagination">
                  <Button
                    size="sm"
                    disabled={device.offset === 0}
                    onClick={device.previous}
                  >
                    Previous 64
                  </Button>
                  <span>
                    {device.total ? device.offset + 1 : 0}–
                    {Math.min(device.offset + 64, device.total)} /{" "}
                    {device.total}
                  </span>
                  <Button
                    size="sm"
                    disabled={device.offset + 64 >= device.total}
                    onClick={device.next}
                  >
                    Next 64
                  </Button>
                  <Button size="sm" onClick={device.refresh}>
                    Refresh membership
                  </Button>
                </div>
              </>
            ) : query ||
              mode === "archived" ||
              device.preferences.view === "flat" ? (
              <>
                {(query ? searchEntries : flatEntries).map(row)}
                {awaitingQuery && <p role="status" className="hint">Searching conversations…</p>}
                {nav && !awaitingQuery && !(query ? searchEntries : flatEntries).length && (
                  <p className="hint">
                    {nav.more
                      ? "Continue searching the remaining conversations."
                      : "No matching conversations."}
                  </p>
                )}
                {nav?.more && !awaitingQuery && (
                  <Button
                    id="sessions-next"
                    size="sm"
                    onClick={() =>
                      run(() =>
                        device.command({
                          action: "navigate",
                          command: { kind: "next", ticket: nav.ticket },
                        }),
                      )
                    }
                  >
                    Continue conversations
                  </Button>
                )}
              </>
            ) : (
              <>
                <WorkspaceNavigation
                  conversationVisible={conversationVisible}
                  nav={nav}
                  device={device}
                  workspaces={workspaces}
                  completeWorkspaces={completeWorkspaces}
                  showUngrouped={showUngrouped}
                  expand={expand}
                  row={row}
                  attention={attention}
                  blankPin={blankPin}
                  reveal={reveal}
                />
                {!completeWorkspaces && catalog?.workspaces_more && (
                  <Button
                    id="workspaces-next"
                    size="sm"
                    onClick={() =>
                      run(() => input.command({ action: "workspaces_next" }))
                    }
                  >
                    More workspaces
                  </Button>
                )}
              </>
            )}
          </div>
        </>
      )}
      <ExternalNavigation />
      {feedback && <Toast key={feedback.sequence} text={feedback.text}
        tone={feedback.success ? "success" : undefined} icon={<IconWarningOutlineRegular/>}
        holdMs={feedback.undo ? 6000 : undefined}
        actions={feedback.undo ? [
          {label:"undo", onClick:() => {setFeedback(undefined);void run(feedback.undo!);}},
          ...(mode === "active" || mode === "attention" ? [{prefix:" or ",label:"filter archived sessions",onClick:() => {setFeedback(undefined);void run(() => selectFilter("all"));}}] : []),
        ] : undefined}
        onDone={() => setFeedback(undefined)}/>}
      {menu && <SessionMenu key={menu.entry.session} target={menu} device={device} reading={attention.get(menu.entry.session)} epoch={nav?.attention_notice ? undefined : nav?.attention?.host_epoch} close={() => setMenu(undefined)}
        archive={nav?.can_archive ? () => archive(menu.entry) : undefined}
        copy={() => void copySessionId(menu.entry.session)} rename={() => setEdit(menu)} mutate={metadata => mutate(menu.entry.session, metadata, menu.revision)}/>}
      {confirmation && <ConfirmArchive {...confirmation} close={() => setConfirmation(undefined)}/>}
      {edit && <RenameSession title={edit.entry.metadata.title ?? titles.get(edit.entry.session) ?? ""}
        close={() => setEdit(undefined)}
        save={title => mutate(edit.entry.session, {...edit.entry.metadata, title: title || null}, edit.revision)}/>}
    </>
  );
}
