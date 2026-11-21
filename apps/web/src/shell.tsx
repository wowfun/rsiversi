import {
  IconPanelLeftOutlineRegular,
  IconNewChatOutlineRegular,
  IconSearchOutlineRegular,
  IconSettingsOutlineRegular,
  IconEllipsisOutlineRegular,
  IconPluginPinwheelOutlineMedium,
  IconClockOutlineRegular,
} from "../vendor/dsh/dockkit-primitives/icons/index.tsx";
import css from "./shell.module.css";
import navigationCss from "./navigation.module.css";
import { DirectoryPickerHost } from "./directory-picker.tsx";
import license from "../vendor/dsh/LICENSE?raw";
import xtermLicense from "@xterm/xterm/LICENSE?raw";
import fitLicense from "@xterm/addon-fit/LICENSE?raw";
import provenance from "../vendor/dsh/provenance.json";
import type { PropsRenderSlots } from "@rsi/dsh-slots";
import {
  lazy,
  Suspense,
  useState,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useSyncExternalStore,
  useMemo,
} from "react";
import { createPortal } from "react-dom";
import {
  usePresentation,
  useResourceVisibility,
  useViewport,
  ResizeHandle,
  Modal,
  CommandPalette,
  LayoutContext,
  hasOpenOverlay,
} from "./presentation.tsx";
import { Button } from "./button.tsx";
import {NavigationSearchButton} from "./navigation.tsx";
import { Brand } from "./brand.tsx";
import { Tooltip } from "../vendor/dsh/dockkit-primitives/Tooltip.tsx";
import {
  input,
  run,
  useView,
  useSelected,
  settingsPanel,
  compactNavigation, dismissNavigationDrawer,
  workbenchPage, pageKind, openPluginPage, loadScheduleCatalog, closeWorkbenchPage,
  connectionStatus,
} from "./bridge.ts";
import { Notices } from "./notices.tsx";
import { Toast } from "../vendor/dsh/dockkit-primitives/Toast.tsx";
import { Setup } from "./setup.tsx";
import settingsCss from "./settings/settings.module.css";
import { Automation } from "./automation.tsx";
import { ResourceCleanupNotice } from "./resource-dock.tsx";
const ScheduleCatalog = lazy(()=>import('./schedule-catalog.tsx'));
const PluginManager = lazy(()=>import('./plugin-manager.tsx'));
const defaultNavigationWidth = 280;
export function Shell({
  renderSlot,
}: PropsRenderSlots<"rsi.navigation" | "rsi.main" | "rsi.resources">) {
  const [automation, setAutomation] = useState(false);
  const page = useSyncExternalStore(workbenchPage.subscribe,pageKind);
  const connection = useSyncExternalStore(
    connectionStatus.subscribe,
    connectionStatus.getSnapshot,
  );
  const settings = useSyncExternalStore(
      settingsPanel.subscribe,
      settingsPanel.getSnapshot,
    ),
    [licenses, setLicenses] = useState(false);
  const setSettings = (tab: NonNullable<typeof settings>) => {
    dismissNavigationDrawer();
    settingsPanel.set(tab);
  };
  const {
      layout: desktopLayout,
      update: updateDesktop,
      expand,
      storageNotice,
      storageRecovery,
      retry,
      reset,
    } = usePresentation();
  const viewport = useViewport();
  const narrow = viewport.width < 768, compact = viewport.width < 1024;
  const selected = useSelected(value=>value);
  const hasResources = useView(view=>!!view?.surfaces[selected]);
  const { layout, update } = useResourceVisibility(
    desktopLayout,
    updateDesktop,
    hasResources,
  );
  const [resourceWidth,setResourceWidth] = useState<number>();
  const [navigationWidth, setNavigationWidth] = useState(defaultNavigationWidth);
  const [deletedTask,setDeletedTask] = useState(0);
  const openingWidth = Math.max(300, Math.round(viewport.width * .45));
  useLayoutEffect(() => {
    if (!layout.resourcesClosed && resourceWidth === undefined) setResourceWidth(openingWidth);
  }, [layout.resourcesClosed, openingWidth, resourceWidth]);
  const layoutContext = useMemo(()=>({layout,update,expand}),[layout,update,expand]);
  const expansion = useSyncExternalStore(compactNavigation.subscribe, compactNavigation.getSnapshot);
  const drawer = narrow && expansion === "drawer";
  const navigationMode = narrow
    ? "hidden"
    : compact
      ? expansion === "column" ? "expanded" : "rail"
      : navigationWidth ? "expanded" : "rail";
  const [palette, setPalette] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const showSessionSearch = useCallback(() => {
    setSearchOpen(true);
    if (narrow) compactNavigation.set("drawer");
    else if (compact) compactNavigation.set("column");
    else setNavigationWidth(width => width || defaultNavigationWidth);
    searchInputRef.current?.focus({preventScroll:true});
  }, [narrow, compact]);
  useLayoutEffect(() => { compactNavigation.set(undefined); }, [narrow, compact]);
  useLayoutEffect(() => {
    if (!layout.resourcesClosed && compact) compactNavigation.set(undefined);
  }, [layout.resourcesClosed, compact]);
  const newConversation = () => void run(() => input.open({action:"new_conversation"}));
  const pageTrigger = useRef<HTMLButtonElement | null>(null);
  const openPage = (destination:'plugins'|'tasks', trigger:HTMLButtonElement) => {
    pageTrigger.current = trigger;
    if (destination === 'plugins') openPluginPage();
    else if (pageKind() !== 'tasks') void loadScheduleCatalog();
    dismissNavigationDrawer();
  };
  const closePage = () => {
    closeWorkbenchPage();
    const trigger = pageTrigger.current;
    if (trigger?.getClientRects().length) trigger.focus();
    else workbench.current?.querySelector<HTMLButtonElement>('[aria-label="Toggle navigation"], [aria-label="Expand navigation"]')?.focus();
  };
  const hasAutomation = useView((view) => !!view?.automation);
  const connected = useView((view) => !!view),
    preferenceError = useView((view) => view?.preference_error);
  const workbench = useRef<HTMLElement>(null),
    loginHeader = useRef<HTMLDivElement>(null),
    [chrome] = useState(() => document.createElement("div"));
  useLayoutEffect(() => {
    workbench.current?.style.setProperty(
      "--navigation-width",
      `${navigationWidth || defaultNavigationWidth}px`,
    );
    workbench.current?.style.setProperty(
      "--resources-width",
      `${resourceWidth ?? openingWidth}px`,
    );
  }, [navigationWidth, resourceWidth, openingWidth]);
  useLayoutEffect(() => {
    (connected
      ? document.getElementById("connection-controls-host")
      : loginHeader.current
    )?.append(chrome);
  }, [connected, chrome]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (!connected || event.defaultPrevented || event.repeat || event.isComposing || event.key === "Dead"
        || event.getModifierState("AltGraph") || event.code !== "KeyK" || event.shiftKey || event.ctrlKey === event.metaKey) return;
      if (event.altKey && (event.metaKey !== /Mac/.test(navigator.platform) ||
        (event.target instanceof Element && event.target.closest(".xterm")) ||
        hasOpenOverlay(document.querySelector("dialog.navigation-drawer[open]")))) return;
      event.preventDefault();
      if (event.altKey) showSessionSearch();
      else setPalette(value => !value);
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [connected, showSessionSearch]);
  useEffect(() => {
    if (!connected) {
      closeWorkbenchPage();
      setPalette(false);
      setSearchOpen(false);
      compactNavigation.set(undefined);
      input.dismissSettings();
    }
  }, [connected]);
  const navigation = () => {
    if (!narrow && navigationMode === "expanded") setSearchOpen(false);
    if (compact) compactNavigation.set(expansion ? undefined : narrow ? "drawer" : "column");
    else setNavigationWidth(width => width ? 0 : defaultNavigationWidth);
  };
  const resources = () => update({ resourcesClosed: !layout.resourcesClosed });
  const appearance = () => setSettings("appearance");
  const sidebar = (
    <>
      <div className="sidebar-brand">
        <Brand />
        <Button size="sm" aria-label="Collapse navigation" data-navigation-initial-focus={searchOpen ? undefined : ""} onClick={navigation}>
          <IconPanelLeftOutlineRegular />
        </Button>
      </div>
      <Button
        className="new-conversation"
        data-testid="new-conversation"
        variant="outline"
        onClick={newConversation}
      >
        <IconNewChatOutlineRegular />
        New session
      </Button>
      <nav className={css.shortcuts} aria-label="Workspace tools">
        <Button aria-label="Open plugins" aria-pressed={page==='plugins'} onClick={e=>openPage('plugins',e.currentTarget)}><IconPluginPinwheelOutlineMedium/>Plugins</Button>
        <Button aria-pressed={page==='tasks'} onClick={e=>openPage('tasks',e.currentTarget)}><IconClockOutlineRegular/>Automation tasks</Button>
        {hasAutomation && <Button onClick={()=>{dismissNavigationDrawer();setAutomation(true)}}><IconClockOutlineRegular/>Deployment checks</Button>}
      </nav>
      {renderSlot("rsi.navigation", {searchOpen, setSearchOpen, searchInputRef, conversationVisible: !page})}
      <div className={`sidebar-footer ${css.footer}`}>
        <details className={`application-options ${css.footerOptions}`}>
          <summary aria-label="More">
            <IconEllipsisOutlineRegular /> More
          </summary>
          <div className={css.menu}>
            <Button id="settings-open" size="sm" onClick={() => setSettings("appearance")}>
              <IconSettingsOutlineRegular /> Settings
            </Button>
            <Button size="sm" onClick={() => setLicenses(true)}>
              Licenses
            </Button>
            <Button size="sm" data-testid="disconnect" disabled={connection.closing} onClick={()=>input.disconnect()}>
              {connection.native ? "Close application" : "Disconnect"}
            </Button>
            <Button size="sm" onClick={reset}>
              Reset layout
            </Button>
          </div>
        </details>
      </div>
    </>
  );
  return (
    <LayoutContext.Provider value={layoutContext}>
      <DirectoryPickerHost />
      {automation && (
        <Modal
          label="Deployment checks"
          close={() => setAutomation(false)}
          className="automation-modal"
        >
          <Automation close={() => setAutomation(false)} />
        </Modal>
      )}
      <div ref={loginHeader} className="login-chrome" />
      {createPortal(
        <header
          role="banner"
          className={`app-header${connected ? " connected-header" : ""}`}
        >
          <Brand />
          {connected && (
            <div className="shell-actions">
              <Button
                size="sm"
                aria-label="Commands"
                title="Commands (Ctrl/Cmd+K)"
                onClick={() => setPalette(true)}
              >
                <IconSearchOutlineRegular />
              </Button>
              {hasResources && <Button
                size="sm"
                aria-label="Toggle resources"
                aria-expanded={!layout.resourcesClosed}
                onClick={resources}
              >
                <IconPanelLeftOutlineRegular className={css.resourcesIcon} />
              </Button>}
            </div>
          )}
          <span
            id="connection-state"
            hidden={connected}
            className={`connection-state${connection.phase === "Connected" ? " connected" : ""}`}
            role="status"
          >
            {connection.phase}
          </span>
        </header>,
        chrome,
      )}
      <Notices />
      {deletedTask > 0 && <Toast key={deletedTask} tone="success" text="Task deleted." onDone={()=>setDeletedTask(0)}/>}
      {connected && <ResourceCleanupNotice />}
      <main
        id="login"
        className="login"
        hidden={connection.phase === "Connected"}
      >
        <div className="login-heading">
          <h1>Open your workspace.</h1>
          <p>
            Use a device receipt to connect this browser to your RSIversi
            service.
          </p>
        </div>
        <form id="login-form" className="login-form">
          <label htmlFor="receipt">Device registration receipt</label>
          <textarea
            id="receipt"
            rows={6}
            spellCheck={false}
            autoComplete="off"
            placeholder="Paste your device receipt"
          />
          <p className="hint">
            On the service computer, run{" "}
            <code>rsi run devices -- register browser</code>.
          </p>
          <label
            id="dev-http-label"
            className="check"
            hidden={!connection.developmentHttp}
          >
            <input id="dev-http" type="checkbox" />
            Allow local HTTP for development
          </label>
          <div className="actions">
            <button
              id="connect"
              className="primary"
              type="submit"
              disabled={connection.busy}
            >
              Connect
            </button>
            <button
              id="reconnect"
              type="button"
              hidden={!connection.reconnect}
              disabled={connection.busy}
            >
              Reconnect with this browser
            </button>
          </div>
          <p className="hint">
            The receipt is used once. Device tokens are not saved in browser
            storage.
          </p>
        </form>
      </main>
      <div
        className="preference-error"
        role="alert"
        hidden={!preferenceError && !storageNotice}
      >
        {[preferenceError, storageNotice].filter(Boolean).join(" ")}
        {storageNotice && (
          <div className="actions">
            {storageRecovery === "reload" ? (
              <Button onClick={() => location.reload()}>Reload window</Button>
            ) : (
              <>
                <Button onClick={retry}>Retry layout</Button>
                {storageRecovery === "reset" && (
                  <Button onClick={reset}>Reset layout</Button>
                )}
              </>
            )}
          </div>
        )}
      </div>
      <main
        ref={workbench}
        id="workbench"
        className={`workbench navigation-${navigationMode}${layout.resourcesClosed ? " resources-closed" : ""}`}
        hidden={connection.phase !== "Connected"}
      >
        {!narrow && navigationMode === "expanded" && (
          <aside
            className={`sidebar ${css.sidebar} ${navigationCss.scope}`}
            aria-label="Workspace navigation"
          >
            {sidebar}
            <ResizeHandle
              name="navigation"
              value={navigationWidth || defaultNavigationWidth}
              min={264}
              max={420}
              onChange={setNavigationWidth}
            />
          </aside>
        )}
        {!narrow && navigationMode === "rail" && (
          <aside className="navigation-rail" aria-label="Workspace navigation">
            <Tooltip label="Expand navigation" delayMs={500} side="right" portal>
              <button
                type="button"
                className={css.railToggle}
                aria-label="Expand navigation"
                aria-expanded={false}
                onClick={navigation}
              >
                <span className={css.railMark} aria-hidden="true"><Brand compact /></span>
                <IconPanelLeftOutlineRegular className={css.railPanel} size={18} />
              </button>
            </Tooltip>
            <Button aria-label="New conversation" onClick={newConversation}>
              <IconNewChatOutlineRegular />
            </Button>
            <Button aria-label="Open plugins" aria-pressed={page==='plugins'} onClick={e=>openPage('plugins',e.currentTarget)}><IconPluginPinwheelOutlineMedium/></Button>
            <Button aria-label="Automation tasks" aria-pressed={page==='tasks'} onClick={e=>openPage('tasks',e.currentTarget)}><IconClockOutlineRegular/></Button>
            <NavigationSearchButton rail open={showSessionSearch}/>
            <Button
              aria-label="Settings"
              onClick={() => setSettings("appearance")}
            >
              <IconSettingsOutlineRegular />
            </Button>
          </aside>
        )}
        {renderSlot("rsi.main", {navigationOpener: connected && narrow && (
          <Button size="sm" aria-label="Toggle navigation" aria-expanded={drawer} onClick={navigation}>
            <IconPanelLeftOutlineRegular size={16}/>
          </Button>
        )})}
        {page==='tasks' && <Suspense fallback={<p role="status">Loading tasks…</p>}><ScheduleCatalog onClose={closePage} onDeleted={()=>setDeletedTask(value=>value+1)}/></Suspense>}
        {page==='plugins' && <Suspense fallback={<p role="status">Loading plugins…</p>}><PluginManager onClose={closePage} onSettings={()=>{closeWorkbenchPage();setSettings('plugins')}}/></Suspense>}
        {!page && <aside className="resources" aria-label="Session resources">
          <ResizeHandle
            name="resources"
            value={Math.min(resourceWidth ?? openingWidth, viewport.width * .7)}
            min={300}
            max={viewport.width * 0.7}
            reverse
            onChange={setResourceWidth}
          />
          {renderSlot("rsi.resources", {viewport})}
        </aside>}
      </main>
      {connected && narrow && (
        <Modal
          className="navigation-drawer"
          label="Workspace navigation"
          open={drawer}
          initialFocus="[data-navigation-initial-focus]"
          fallbackFocus='[aria-label="Expand navigation"], [aria-label="Toggle navigation"], .workbench > .sidebar [aria-label="Collapse navigation"]'
          lightDismiss
          close={dismissNavigationDrawer}
        >
          <div className={`sidebar ${css.sidebar} ${navigationCss.scope}`}>{sidebar}</div>
        </Modal>
      )}
      {palette && connected && (
        <CommandPalette
          close={() => setPalette(false)}
          actions={[
            {
              label: "Focus conversation composer",
              run: () =>
                document
                  .querySelector<HTMLTextAreaElement>(
                    ".pane.selected textarea:not([disabled])",
                  )
                  ?.focus(),
            },
            { label: "Toggle navigation", run: navigation },
            ...(hasResources ? [{ label: "Toggle resources", run: resources }] : []),
            { label: "Open Settings", run: () => setSettings("appearance") },
            { label: connection.native ? "Close application" : "Disconnect", run: () => {void run(()=>input.disconnect())} },
            { label: "View licenses", run: () => setLicenses(true) },
          ]}
        />
      )}
      {licenses && (
        <Modal
          className="license-dialog"
          label="Third-party software"
          close={() => setLicenses(false)}
        >
          <h2>Third-party software</h2>
          <p>
            DeepSeek Harness · MIT ·{" "}
            {[...new Set(provenance.files.map((file) => file.revision))]
              .map((revision) => revision.slice(0, 7))
              .join(", ")}
          </p>
          <pre>{license}</pre>
          <p>xterm.js 6.0.0 · MIT</p>
          <pre>{xtermLicense}</pre>
          <p>xterm Fit 0.11.0 · MIT</p>
          <pre>{fitLicense}</pre>
          <Button onClick={() => setLicenses(false)}>Close licenses</Button>
        </Modal>
      )}
      {connected && settings && (
        <Modal
          className={`settings-modal ${settingsCss.modal}`}
          label="Settings"
          close={() => input.dismissSettings()}
        >
          <Setup tab={settings} close={() => input.dismissSettings()} />
        </Modal>
      )}
      <div id="resource-parking" hidden />
    </LayoutContext.Provider>
  );
}
