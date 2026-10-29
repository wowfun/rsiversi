import { ConversationRow } from "./navigation-row.tsx";
import { WorkspaceNavigation } from "./navigation-workspaces.tsx";
import {NavigationOptions, type ConversationFilter} from "./navigation-options.tsx";
import { AttentionNavigation } from "./navigation-attention.tsx";
import {
  IconSearchOutlineRegular,
  IconProjectAddOutlineRegular,
  IconCloseOutlineRegular,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import css from "./navigation.module.css";
import { useLayout } from "./presentation.tsx";
import { useDeviceNavigation } from "./navigation-device.ts";
import { useEffect, useMemo, useRef, useState } from "react";
import { chooseWorkspace } from "./directory-picker.tsx";
import { Button } from "./button.tsx";
import { input, run, useView, type NavigationEntry } from "./bridge.ts";
import {
  navigationPresentation,
  GroupRestoration,
} from "../navigation-presentation.js";
export function Navigation() {
  const [now, setNow] = useState(Date.now);
  useEffect(()=>{const timer=setInterval(()=>setNow(Date.now()), 60_000);return ()=>clearInterval(timer);}, []);
  const nav = useView((value) => value?.navigation),
    catalog = useView((value) => value?.catalog);
  const selectedWorkspaces = useView((value) =>
    JSON.stringify(
      [
        ...new Set(
          Object.values(value?.surfaces ?? {})
            .map((surface) => surface?.workspace)
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
  const device = useDeviceNavigation(nav, workspaces, !completeWorkspaces);
  const restoring = useRef(new GroupRestoration());
  const restoreKey = useMemo(() => {
    const registered = new Set(workspaces.map((row) => row.id)),
      collapsed = new Set(
        layout.workspaces
          .filter((item) => !item.expanded)
          .map((item) => item.id),
      );
    const desired = new Set(
      layout.workspaces.filter((item) => item.expanded).map((item) => item.id),
    );
    for (const id of JSON.parse(selectedWorkspaces) as string[])
      if (registered.has(id) && !collapsed.has(id)) desired.add(id);
    return JSON.stringify(
      [...desired]
        .filter(
          (id) => !nav?.groups[id] && (id === "other" || registered.has(id)),
        )
        .sort(),
    );
  }, [workspaces, layout.workspaces, selectedWorkspaces, nav?.groups]);
  useEffect(() => {
    if (!nav?.ticket) return;
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
  }, [restoreKey, nav?.ticket]);
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState(""),
    [mode, setMode] = useState<ConversationFilter>("active");
  const [edit, setEdit] = useState<NavigationEntry>(),
    [editRevision, setEditRevision] = useState<string>(),
    [title, setTitle] = useState("");
  const filter = {
    query,
    archived: mode === "all" ? null : mode === "archived",
    workspace: { kind: "all" },
  };
  const search = (value = filter) =>
    device.command({
      action: "navigate",
      command: { kind: "query", filter: value },
    });
  const mutate = (session: string, metadata: NavigationEntry["metadata"], revision = nav?.metadata_revision) =>
    device.command({
      action: "navigate",
      command: { kind: "replace", ticket: nav?.ticket, session, metadata, metadata_revision: revision },
    });
  const presentation = useMemo(
    () =>
      navigationPresentation(
        [
          ...(nav?.entries ?? []),
          ...Object.values(nav?.groups ?? {}).flatMap((group) => group.entries),
          ...(nav?.pins ?? []).flatMap((pin) =>
            pin.status === "available" ? [pin.entry] : [],
          ),
          ...(nav?.order?.entries ?? []).flatMap((entry) =>
            entry ? [entry] : [],
          ),
        ],
        nav?.attention,
      ),
    [nav?.entries, nav?.groups, nav?.pins, nav?.order?.entries, nav?.attention],
  );
  const row = (entry: NavigationEntry) => (
    <ConversationRow
      now={now}
      key={entry.session}
      entry={entry}
      title={presentation.titles.get(entry.session)}
      activity={presentation.attention.get(entry.session)}
      device={device}
      mutate={mutate}
      onRename={(entry) => {
        setEdit(entry);
        setEditRevision(nav?.metadata_revision);
        setTitle(entry.metadata.title ?? "");
      }}
    />
  );
  const pins = nav?.pins ? [...nav.pins] : [];
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
        {searchOpen ? <form className="navigation-search" onSubmit={event=>{event.preventDefault();void run(()=>search())}}>
          <input autoFocus aria-label="Search conversations" placeholder="Search conversations" value={query} onChange={event=>setQuery(event.target.value)} onKeyDown={event=>{if(event.key==='Escape'){event.stopPropagation();setSearchOpen(false);setQuery('');void run(()=>search({...filter,query:''}))}}}/>
          <Button size="sm" type="submit" aria-label="Search"><IconSearchOutlineRegular size={16}/></Button>
          <Button size="sm" aria-label="Close search" onClick={()=>{setSearchOpen(false);setQuery('');return search({...filter,query:''})}}><IconCloseOutlineRegular size={16}/></Button>
        </form> : <>
          <h2 data-testid="workspace-heading">Workspaces</h2>
          <div className="navigation-actions">
            <Button size="sm" aria-label="Search conversations" onClick={()=>setSearchOpen(true)}><IconSearchOutlineRegular size={16}/></Button>
            <NavigationOptions device={device} filter={mode} onFilter={async value=>{if(value!=='attention')await search({...filter,archived:value==='all'?null:value==='archived'});setMode(value)}} refresh={async()=>{await input.command({action:'refresh'});await search()}}/>
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
      {mode === "attention" ? (
        <AttentionNavigation nav={nav} />
      ) : (
        <>
          {!!nav?.pins.length && (
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
            {device.manual ? (
              <>
                {device.loading ? (
                  <p role="status" className="hint">
                    Loading conversations…
                  </p>
                ) : device.preferences.view === "flat" ? (
                  device.entries.map(row)
                ) : (
                  <WorkspaceNavigation nav={nav} device={device} workspaces={workspaces} completeWorkspaces={completeWorkspaces} expand={expand} row={row}/>
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
                {nav?.entries.map(row)}
                {nav && !nav.entries.length && (
                  <p className="hint">
                    {nav.more
                      ? "Continue searching the remaining conversations."
                      : "No matching conversations."}
                  </p>
                )}
                {nav?.more && (
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
                  nav={nav}
                  device={device}
                  workspaces={workspaces}
                  completeWorkspaces={completeWorkspaces}
                  expand={expand}
                  row={row}
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
      {edit && (
        <form
          className="rename-session"
          onSubmit={(event) => {
            event.preventDefault();
            void run(async () => {
              await mutate(edit.session, {
                ...edit.metadata,
                title: title || null,
              }, editRevision);
              setEdit(undefined);
            });
          }}
        >
          <label>
            Conversation title
            <input
              aria-label="Conversation title"
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              maxLength={256}
            />
          </label>
          <Button type="submit" size="sm">
            Save title
          </Button>
          <Button size="sm" onClick={() => setEdit(undefined)}>
            Cancel
          </Button>
        </form>
      )}
    </>
  );
}
