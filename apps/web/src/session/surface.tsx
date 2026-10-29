import {WorkspaceChoice} from './workspace-choice.tsx';
import { ExternalSurface } from "./external.tsx";
import { IconEllipsisOutlineRegular, IconChevronDownOutlineRegular } from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import { HistoryPicker } from "./history-picker.tsx";
import { FilePicker } from "./file-picker.tsx";
import { SessionExtensions, SurfaceMenu } from "./extensions.tsx";
import { SavedDrafts } from "./saved-drafts.tsx";
import css from "./surface.module.css";
import chat from "./chat.module.css";
import { ConversationWidthControls } from "../conversation-width.tsx";
import { ReferencePicker } from "./reference-picker.tsx";
import { QueueEditor } from "./queue-editor.tsx";
import { Composer } from "./composer.tsx";
import { Transcript } from "./transcript.tsx";
import {SessionMetrics} from './metrics.tsx';
import { useLayoutEffect, useRef, useId, useState, useSyncExternalStore } from "react";
import {
  PendingInputs,
  PendingInteractions,
  SessionCommands,
} from "./projections.tsx";
import { useSelected, useView } from "../bridge.ts";
import { sessionSurfaces, type SessionSurfaceOwner } from "./state.ts";

function NativeSurface({
  owner,
  selected,
  mode,
  column,
}: {
  owner: SessionSurfaceOwner;
  selected: boolean;
  mode: string;
  column: HTMLElement | null;
}) {
  useSyncExternalStore(owner.subscribe, owner.getSnapshot);
  const data = owner.staged?.data;
  const empty = !data?.transcript.blocks.length;
  const [tab, setTab] = useState("conversation");
  const tabId = useId();
  const coordinates = data
    ? `${data.path} · ${data.session}`
    : "Select a workspace to begin";
  const remote = useView((view) => view?.has_remote_ui);
  const elements = useRef<Record<string, HTMLElement>>({});
  const refs = useRef<Record<string, (node: HTMLElement | null) => void>>({});
  const ref = (name: string) =>
    (refs.current[name] ??= (node) => {
      if (node) elements.current[name] = node;
    });
  const label = owner.index === "compare" ? "Compare" : "Main",
    hintId = useId();
  useLayoutEffect(() => {
    owner.mount(elements.current);
    return () => owner.unmount();
  }, [owner]);
  useLayoutEffect(() => owner.applyFrame(), [owner, owner.staged]);
  const control = (
    name: string,
    text: string,
    className = "quiet",
    hidden = false,
    disabled = false,
  ) => (
    <button
      ref={ref(name)}
      type="button"
      className={className}
      hidden={hidden}
      disabled={disabled}
    >
      {text}
    </button>
  );
  return (
    <section
      ref={ref("node")}
      className={`${css.scope} ${chat.scope} pane${selected ? " selected" : ""}${empty ? " is-empty" : ""}`}
      data-session-id={data?.session ?? ""}
      hidden={!selected}
      aria-label={`${label} conversation`}
    >
      <header className="pane-header" hidden={empty}>
        <div className="pane-heading">
          <span className="pane-status" hidden>{data?.historical ? "History" : data?.transcript.status || "Ready"}</span>
          <nav className="transcript-tabs" role="tablist" aria-label="Conversation view" hidden={empty}>
            {['conversation','activity'].map(value=><button key={value} type="button" role="tab" id={`${tabId}-${value}`} aria-controls={`${tabId}-panel`} aria-selected={tab===value} tabIndex={tab===value?0:-1} onClick={()=>setTab(value)} onKeyDown={event=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(event.key)){event.preventDefault();const next=event.key==='Home'?'conversation':event.key==='End'?'activity':tab==='conversation'?'activity':'conversation';setTab(next);document.getElementById(`${tabId}-${next}`)?.focus();}}}>{value==='conversation'?'Chat':'Trajectory'}</button>)}
          </nav>
          <details className="session-menu">
            <summary aria-label="Session tools">
              <IconEllipsisOutlineRegular />
            </summary>
            <div className="pane-tools">
              <p className="pane-session mono">{coordinates}</p>
              {control(
                "history",
                "Earlier history",
                "quiet",
                false,
                !data || (!data.history_more && !!data.historical),
              )}
              {control("live", "Back to live", "quiet", !data?.historical)}
              {control(
                "commands",
                "Session commands",
                "quiet",
                false,
                !data || !!data.protected || owner.switching,
              )}
              <SurfaceMenu data={data} owner={owner} remote={remote} />
              <SessionExtensions data={data} />
            </div>
          </details>
        </div>
      </header>
      <SessionCommands data={data} owner={owner} />
      <SavedDrafts state={owner.recoveryView} />
      <div className="welcome" hidden={!empty}>
        <h1>What will you build?</h1>

      </div>
      <div className={chat.transcriptArea} hidden={empty} role="tabpanel" id={`${tabId}-panel`} aria-labelledby={`${tabId}-${tab}`}>
        <Transcript
          register={ref("transcript")}
          owner={owner}
          transcript={owner.staged?.data?.transcript ?? { blocks: [] }}
          generation={owner.staged?.data?.generation}
          cards={!!owner.staged?.data?.ui_cards}
          inline={data?.inline}
          mode={tab === "activity" ? "verbose" : mode}
        />
        <button ref={ref("bottom")} type="button" className={`back-bottom ${chat.bottom}`}
          aria-label="Back to bottom" title="Back to bottom" hidden={!owner.readingSummary}>
          <IconChevronDownOutlineRegular />
        </button>
        {selected && <ConversationWidthControls container={column} showHandles={!empty} />}
        
      </div>
      <PendingInputs data={data} owner={owner} register={ref("queueList")} />
      <PendingInteractions data={data} owner={owner} />
      <div className="pane-notice">{data?.notice}</div>
      <div className="pane-notice" hidden={!data?.protected}>
        <p>
          Protected investigation. Use Deployment checks to cancel or create a
          new attempt.
        </p>
        {control("protectedExport", "Export investigation")}
      </div>
      {owner.historyDialog && (
        <HistoryPicker
          key={`history:${owner.historyDialog.id}`}
          state={owner.historyDialog}
        />
      )}
      {owner.fileDialog && (
        <FilePicker key={`file:${owner.fileDialog.id}`} state={owner.fileDialog} />
      )}
      {owner.referenceDialog && (
        <ReferencePicker
          key={`reference:${owner.referenceDialog.id}`}
          state={owner.referenceDialog}
        />
      )}
      {owner.queueDialog && (
        <QueueEditor key={`queue:${owner.queueDialog.id}`} state={owner.queueDialog} />
      )}
      <div className="composer-context" hidden={!empty}><WorkspaceChoice owner={owner} workspace={data?.workspace ?? owner.composerView?.workspace}/></div>
      <Composer
        owner={owner}
        data={data}
        models={owner.staged?.models ?? []}
        submitKey={owner.staged?.submitKey ?? "enter"}
        register={ref}
        hintId={hintId}
      />
      <div className="session-metrics-seat">
        {data && <SessionMetrics key={`metrics:${data.generation}`} owner={owner}/>}
      </div>
    </section>
  );
}

export function SessionSurfaces({
  mode,
  column,
}: {
  mode: string;
  column: HTMLElement | null;
}) {
  const owners = useSyncExternalStore(
    sessionSurfaces.subscribe,
    sessionSurfaces.getSnapshot,
  );
  const selected = useSelected((value) => value);
  return (
    <>
      {owners.map((owner) =>
        owner.kind === "external" ? (
          <ExternalSurface
            key={`${owner.index}:external`}
            owner={owner}
            selected={owner.index === selected}
          />
        ) : (
          <NativeSurface
            key={owner.index}
            owner={owner}
            selected={owner.index === selected}
            mode={mode}
            column={column}
          />
        ),
      )}
    </>
  );
}
