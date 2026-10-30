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
import { DirectoryPickerHost, chooseWorkspace } from "./directory-picker.tsx";
import license from "../vendor/dsh/LICENSE?raw";
import xtermLicense from "@xterm/xterm/LICENSE?raw";
import fitLicense from "@xterm/addon-fit/LICENSE?raw";
import provenance from "../vendor/dsh/provenance.json";
import type { PropsRenderSlots } from "@rsi/dsh-slots";
import {
  useState,
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
  useNarrow,
  useViewport,
  ResizeHandle,
  Modal,
  CommandPalette,
  LayoutContext,
} from "./presentation.tsx";
import { Button } from "./button.tsx";
import { Brand } from "./brand.tsx";
import { Tooltip } from "../vendor/dsh/dockkit-primitives/Tooltip.tsx";
import {
  input,
  run,
  useView,
  useSelected,
  settingsPanel,
  connectionStatus,
} from "./bridge.ts";
import { Notices } from "./notices.tsx";
import { Setup } from "./setup.tsx";
import settingsCss from "./settings/settings.module.css";
import { Automation } from "./automation.tsx";
import { ResourceDocks, ResourceCleanupNotice } from "./resource-dock.tsx";
export function Shell({
  renderSlot,
}: PropsRenderSlots<"rsi.navigation" | "rsi.main" | "rsi.resources">) {
  const [automation, setAutomation] = useState(false);
  const connection = useSyncExternalStore(
    connectionStatus.subscribe,
    connectionStatus.getSnapshot,
  );
  const settings = useSyncExternalStore(
      settingsPanel.subscribe,
      settingsPanel.getSnapshot,
    ),
    setSettings = settingsPanel.set,
    [licenses, setLicenses] = useState(false);
  const {
      layout: desktopLayout,
      update: updateDesktop,
      expand,
      storageNotice,
      storageRecovery,
      retry,
      reset,
    } = usePresentation(),
    narrow = useNarrow(),
    compact = useNarrow("(max-width: 1023px)");
  const selected = useSelected(value=>value);
  const hasResources = useView(view=>!!view?.surfaces[selected]);
  const { layout, update } = useResourceVisibility(
    desktopLayout,
    updateDesktop,
    narrow,
    hasResources,
  );
  const viewport = useViewport();
  const layoutContext = useMemo(()=>({layout,update,expand}),[layout,update,expand]);
  const navigationMode = narrow
    ? "hidden"
    : compact && layout.navigation === "expanded"
      ? "rail"
      : layout.navigation;
  const [drawer, setDrawer] = useState(false),
    [palette, setPalette] = useState(false);
  const newConversation = () => void run(() => input.open({action:"new_conversation"}));
  const hasAutomation = useView((view) => !!view?.automation);
  const connected = useView((view) => !!view),
    preferenceError = useView((view) => view?.preference_error);
  const workbench = useRef<HTMLElement>(null),
    loginHeader = useRef<HTMLDivElement>(null),
    [chrome] = useState(() => document.createElement("div"));
  useLayoutEffect(() => {
    workbench.current?.style.setProperty(
      "--navigation-width",
      `${layout.navigationWidth}px`,
    );
    workbench.current?.style.setProperty(
      "--resources-width",
      `${layout.resourcesWidth * 100}vw`,
    );
  }, [layout]);
  useLayoutEffect(() => {
    (connected
      ? document.getElementById("connection-controls-host")
      : loginHeader.current
    )?.append(chrome);
  }, [connected, chrome]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (
        (event.ctrlKey || event.metaKey) &&
        event.key.toLowerCase() === "k" &&
        !event.isComposing &&
        connected
      ) {
        event.preventDefault();
        setPalette((value) => !value);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [connected]);
  useEffect(() => {
    if (!connected) {
      setPalette(false);
      setDrawer(false);
      input.dismissSettings();
    }
  }, [connected]);
  const navigation = () =>
    drawer || compact
      ? setDrawer((value) => !value)
      : update({
          navigation: layout.navigation === "expanded" ? "rail" : "expanded",
        });
  const resources = () => update({ resourcesClosed: !layout.resourcesClosed });
  const appearance = () => setSettings("appearance");
  const sidebar = (
    <>
      <div className="sidebar-brand">
        <Brand />
        <Button size="sm" aria-label="Collapse navigation" onClick={navigation}>
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
        <Button aria-label="Open plugins" onClick={()=>setSettings('plugins')}><IconPluginPinwheelOutlineMedium/>Plugins</Button>
        {hasAutomation && <Button onClick={()=>setAutomation(true)}><IconClockOutlineRegular/>Deployment checks</Button>}
      </nav>
      {renderSlot("rsi.navigation", {})}
      <div className={`sidebar-footer ${css.footer}`}>
        <details className={`application-options ${css.footerOptions}`}>
          <summary aria-label="More">
            <IconEllipsisOutlineRegular /> More
          </summary>
          <div className={css.menu}>
            <Button id="settings-open" size="sm" onClick={() => {setSettings("appearance");setDrawer(false);}}>
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
              {narrow && <Button
                size="sm"
                aria-label="Toggle navigation"
                aria-expanded={compact ? drawer : navigationMode === "expanded"}
                onClick={navigation}
              >
                <IconPanelLeftOutlineRegular />
              </Button>}
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
              value={layout.navigationWidth}
              min={264}
              max={420}
              onChange={(navigationWidth) => update({ navigationWidth })}
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
                aria-expanded={drawer}
                onClick={() => compact ? setDrawer(true) : update({navigation:"expanded"})}
              >
                <span className={css.railMark} aria-hidden="true"><Brand compact /></span>
                <IconPanelLeftOutlineRegular className={css.railPanel} size={18} />
              </button>
            </Tooltip>
            <Button aria-label="New conversation" onClick={newConversation}>
              <IconNewChatOutlineRegular />
            </Button>
            <Button
              aria-label="Settings"
              onClick={() => setSettings("appearance")}
            >
              <IconSettingsOutlineRegular />
            </Button>
          </aside>
        )}
        {renderSlot("rsi.main", {})}
        <aside className="resources" aria-label="Session resources">
          <ResizeHandle
            name="resources"
            value={layout.resourcesWidth * viewport.width}
            min={300}
            max={viewport.width * 0.7}
            reverse
            onChange={(resourcesWidth) =>
              update({ resourcesWidth: resourcesWidth / viewport.width })
            }
          />
          <ResourceDocks launcher={renderSlot("rsi.resources", {})} />
        </aside>
      </main>
      {connected && drawer && (
        <Modal
          className="navigation-drawer"
          label="Workspace navigation"
          close={() => setDrawer(false)}
        >
          <button onClick={() => setDrawer(false)}>Close navigation</button>
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
