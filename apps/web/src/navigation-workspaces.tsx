import {WorkspaceHeader} from "./workspace-header.tsx";
import { useMemo, useState, type ReactNode } from "react";
import {visibleConversations} from '../navigation-presentation.js';
import {
  IconChevronDownOutlineRegular,
  IconChevronRightOutlineRegular,
  IconFolderCloseRegular,
  IconFolderOpenOutlineRegular,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { workspaceTree } from "../workspace-tree.js";
import { Button } from "./button.tsx";
import { sessionSurfaces } from "./session/state.ts";
import {
  closeScheduleCatalog,
  dismissNavigationDrawer,
  run,
  useSelected,
  useView,
  type Navigation,
  type NavigationEntry,
  type WorkspaceFilter,
} from "./bridge.ts";
import type { useDeviceNavigation } from "./navigation-device.ts";
import type { useLayout } from "./presentation.tsx";
import {workspaceName as basename} from '../workspace-name.js';
export function WorkspaceNavigation({
  nav,
  device,
  workspaces,
  completeWorkspaces,
  showUngrouped,
  expand,
  row,
}: {
  nav: Navigation | undefined;
  device: ReturnType<typeof useDeviceNavigation>;
  workspaces: (Parameters<typeof useDeviceNavigation>[1][number] & {name?:string|null;created_at_ms?:number|null})[];
  completeWorkspaces: boolean;
  showUngrouped: boolean;
  expand: ReturnType<typeof useLayout>["expand"];
  row(entry: NavigationEntry): ReactNode;
}) {
  const selected = useSelected(value => value);
  const active = useView(value => {const surface=value?.surfaces[selected];return surface?.kind === "native" ? surface : undefined;});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
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
    const visible = visibleConversations(page?.entries ?? [], expanded.has(key), active?.session);
    const remaining = (page?.entries.length ?? 0) - visible.length;
    const registration = active?.workspace.registration;
    const draftGroup = registration?.status === 'registered' ? registration.id :
      registration?.status === 'unregistered' ? 'other' : undefined;
    const showDraft = active?.draft && draftGroup === key && nav?.filter.archived !== true &&
      !page?.entries.some(entry => entry.session === active.session);
    return (
      <section
        className="workspace-group"
        key={key}
        data-testid="workspace-group"
        data-workspace-id={key}
      >
        <WorkspaceHeader record={record} name={name}
          className="workspace-row"
          data-active={active?.workspace.registration.status === 'registered' && active.workspace.registration.id === key || undefined}
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
            {showDraft && <div className="session-row" data-session-id={active.session}>
              <button type="button" className="nav-item" aria-current="true" onClick={() => {
                closeScheduleCatalog();
                dismissNavigationDrawer();
                const pane = sessionSurfaces.getSnapshot().find(surface => surface.index === selected);
                if (pane && pane.kind !== 'external') requestAnimationFrame(() => pane.focusInput());
              }}><strong>New session</strong></button>
            </div>}
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
            {visible.map(row)}
            {(remaining > 0 || expanded.has(key) && page.entries.length > 5) && <button type="button" className="workspace-show-more" aria-expanded={expanded.has(key)} onClick={()=>setExpanded(previous=>{const next=new Set(previous);if(next.has(key))next.delete(key);else next.add(key);return next;})}>{remaining > 0 ? `Show ${remaining} more ${remaining === 1 ? 'session' : 'sessions'}` : 'Show less'}</button>}
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
            {!page.stale && !page.entries.length && page.more && (
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
