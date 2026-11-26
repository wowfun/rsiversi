import {WorkspaceHeader} from "./workspace-header.tsx";
import { useMemo, useState, type ReactNode } from "react";
import {sessionWorkspaceGroup, sidebarConversations, workspaceRowWindow} from '../navigation-presentation.js';
import {BlankConversationRow} from './navigation-row.tsx';
import {
  IconChevronDownOutlineRegular,
  IconChevronRightOutlineRegular,
  IconFolderCloseRegular,
  IconFolderOpenOutlineRegular,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { workspaceTree } from "../workspace-tree.js";
import { Button } from "./button.tsx";
import {
  run,
  useSelected,
  useView,
  type Navigation,
  type NavigationEntry,
  type AttentionEntry,
  type WorkspaceFilter,
} from "./bridge.ts";
import type { useDeviceNavigation } from "./navigation-device.ts";
import type { useLayout } from "./presentation.tsx";
import {workspaceName as basename} from '../workspace-name.js';
function WorkspaceRows({entries, current, attention, reveal, row}: {
  entries: NavigationEntry[];
  current?: string;
  reveal?: string;
  attention: ReadonlyMap<string, AttentionEntry>;
  row(entry: NavigationEntry): ReactNode;
}) {
  const [stored, setStored] = useState(5);
  const {minimum, limit, visible, canFold} = workspaceRowWindow(entries, current, attention, stored, reveal);
  if (limit > stored) setStored(limit);
  const remaining = entries.length - visible;
  return <>
    {entries.slice(0, visible).map(row)}
    {canFold && <button type="button" className="workspace-show-more" aria-expanded={remaining === 0}
      onClick={() => setStored(remaining === 0 ? minimum : remaining <= 5 ? Infinity : limit + 5)}>
      {remaining === 0 ? 'Show less' : `Show ${remaining} more ${remaining === 1 ? 'session' : 'sessions'}`}
    </button>}
  </>;
}

export function WorkspaceNavigation({
  conversationVisible,
  nav,
  device,
  workspaces,
  completeWorkspaces,
  showUngrouped,
  expand,
  row,
  attention,
  blankPin,
  reveal,
}: {
  conversationVisible: boolean;
  nav: Navigation | undefined;
  device: ReturnType<typeof useDeviceNavigation>;
  workspaces: (Parameters<typeof useDeviceNavigation>[1][number] & {name?:string|null;created_at_ms?:number|null})[];
  completeWorkspaces: boolean;
  showUngrouped: boolean;
  expand: ReturnType<typeof useLayout>["expand"];
  row(entry: NavigationEntry): ReactNode;
  attention: ReadonlyMap<string, AttentionEntry>;
  blankPin?: NavigationEntry;
  reveal?: string;
}) {
  const selected = useSelected(value => value);
  const active = useView(value => {const surface=value?.surfaces[selected];return surface?.kind === "native" ? surface : undefined;});
  const activeGroup = sessionWorkspaceGroup(active, nav);
  const group = (
    key: string,
    name: string,
    workspace: WorkspaceFilter,
    path?: string,
  ) => {
    const registered = workspaces.find(item=>item.id===key);
    const record = registered ? {...registered, coordinates:{path:registered.path,location:registered.location}} : undefined;
    name = record ? basename(record.coordinates.path,record.name) : name;
    const savedPage = nav?.groups[key];
    const page =
      device.manual && savedPage
        ? {
            ...savedPage,
            entries: device.entries.filter((entry) =>
              key === "other"
                ? entry.workspace === null
                : entry.workspace === key,
            ),
            continued: false,
            stale: false,
            more: false,
          }
        : savedPage;
    const entries = page?.entries ?? [];
    const includePin = !device.manual && blankPin && (blankPin.workspace ?? 'other') === key;
    const listed = sidebarConversations(includePin ? [blankPin, ...entries] : entries, active?.session);
    const showDraft = active?.draft && active.creation && activeGroup === key &&
      nav?.filter.archived !== true && !listed.some(entry => entry.session === active.session);
    return (
      <section
        className="workspace-group"
        key={key}
        data-testid="workspace-group"
        data-workspace-id={key}
      >
        <WorkspaceHeader record={record} name={name}
          className="workspace-row"
          data-active={activeGroup === key || undefined}
          draggable={!!path && device.ready && completeWorkspaces}
          onDragStart={(event) => {
            event.stopPropagation();
            event.dataTransfer.setData("application/rsi-workspace", key);
          }}
          onDragOver={(event) => {
            if (event.dataTransfer.types.includes("application/rsi-workspace"))
              event.preventDefault();
          }}
          onDrop={(event) => {
            event.preventDefault();
            const id = event.dataTransfer.getData("application/rsi-workspace");
            if (id) void run(() => device.workspaceMove(id, "before", key));
          }}
          onKeyDown={(event) => {
            if (
              path &&
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
                device.workspaceMove(
                  key,
                  positions[event.key as keyof typeof positions],
                ),
              );
            }
          }}
        >
          <button
            className="nav-item workspace-toggle"
            aria-expanded={!!page}
            onClick={() =>
              run(async () => {
                if (page) expand(key, false);
                await device.command({
                  action: "navigate",
                  command: page
                    ? { kind: "close_group", workspace }
                    : { kind: "group", workspace, ticket: null },
                });
                if (!page) expand(key, true);
              })
            }
          >
            <span className="workspace-icon">
              {page?<IconFolderOpenOutlineRegular/>:<IconFolderCloseRegular/>}
              {page?<IconChevronDownOutlineRegular/>:<IconChevronRightOutlineRegular/>}
            </span>
            <strong>{name}</strong>
          </button>
        </WorkspaceHeader>
        {page && (
          <div className="workspace-conversations">
            {showDraft && <BlankConversationRow session={active.session} active={conversationVisible}/>}
            {page.continued && !page.stale && (
              <p className="hint">
                Earlier entries are outside this page.{" "}
                <button
                  onClick={() =>
                    run(() =>
                      device.command({
                        action: "navigate",
                        command: { kind: "group", workspace, ticket: null },
                      }),
                    )
                  }
                >
                  Back to newest
                </button>
              </p>
            )}
            <WorkspaceRows entries={listed} current={active?.session} attention={attention} reveal={reveal} row={row}/>
            {page.stale && (
              <p className="hint" role="status">
                Conversations changed.{" "}
                <button
                  aria-label={`Refresh conversations in ${name}`}
                  onClick={() =>
                    run(() =>
                      device.command({
                        action: "navigate",
                        command: { kind: "group", workspace, ticket: null },
                      }),
                    )
                  }
                >
                  Refresh this workspace
                </button>
              </p>
            )}
            {!page.stale && !listed.length && !showDraft && page.more && (
              <p className="hint">No matches in this scan. Continue searching.</p>
            )}
            {page.more && (
              <Button
                size="sm"
                aria-label={`Load more in ${name}`}
                onClick={() =>
                  run(() =>
                    device.command({
                      action: "navigate",
                      command: {
                        kind: "group",
                        workspace,
                        ticket: page.ticket,
                      },
                    }),
                  )
                }
              >
                Load more
              </Button>
            )}
          </div>
        )}
      </section>
    );
  };
  type TreeNode = {
    key: string;
    label: string;
    workspace: { id: string; path: string } | null;
    children: TreeNode[];
  };
  const treeNode = (node: TreeNode): React.ReactNode => (
    <div className="workspace-tree-node" key={node.key}>
      {node.workspace ? (
        group(
          node.workspace.id,
          node.label,
          { kind: "registered", id: node.workspace.id },
          node.workspace.path,
        )
      ) : (
        <span className="workspace-tree-label">{node.label}</span>
      )}
      {!!node.children.length && (
        <div className="workspace-tree-children">
          {node.children.map(treeNode)}
        </div>
      )}
    </div>
  );
  const trees = useMemo(
    () =>
      workspaceTree(
        workspaces,
        device.preferences.workspaceOrder === "manual"
          ? device.workspaceIds
          : [],
      ),
    [workspaces, device.preferences.workspaceOrder, device.workspaceIds],
  );
  const visibleWorkspaces = useMemo(() => {
    if (device.preferences.workspaceOrder !== "manual") return workspaces;
    const ranks = new Map(device.workspaceIds.map((id, index) => [id, index]));
    return [...workspaces].sort(
      (a, b) =>
        (ranks.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
        (ranks.get(b.id) ?? Number.MAX_SAFE_INTEGER),
    );
  }, [workspaces, device.preferences.workspaceOrder, device.workspaceIds]);
  return (
    <>
      <div id="workspaces">
        {device.preferences.view === "workspace_tree"
          ? trees.map((location) => (
              <section className="workspace-location" key={location.key}>
                <h3>{location.label}</h3>
                {location.children.map(treeNode)}
              </section>
            ))
          : visibleWorkspaces.map((item) =>
              group(
                item.id,
                basename(item.path),
                { kind: "registered", id: item.id },
                item.path,
              ),
            )}
      </div>
      {showUngrouped && group("other", "Ungrouped", { kind: "unregistered" })}
    </>
  );
}
