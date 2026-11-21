import {DocumentSessionScope, DocumentContributions, DocumentReviews} from "../document-plugins.tsx";
import {closeDisclosure} from '../../disclosure.js';
import {WorkspaceChoice} from './workspace-choice.tsx';
import {PresetChoice, PresetLabel} from './preset-choice.tsx';
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
import { PendingInputs } from "./queue-dock.tsx";
import { Composer } from "./composer.tsx";
import {TodoPanel} from './todo-panel.tsx';
import { Transcript } from "./transcript.tsx";
import {SessionMetrics} from './metrics.tsx';
import {ScheduledTasks} from './scheduled-tasks.tsx';
import {SessionPanel} from './session-panel.tsx';
import { useLayoutEffect, useMemo, useRef, useId, useState, useSyncExternalStore } from "react";
import {presentationIdentity} from '../presentation.tsx';
import {
  PendingInteractions,
  SessionCommands,
} from "./projections.tsx";
import { run, useSelected, useView } from "../bridge.ts";
import { sessionSurfaces, type SessionSurfaceOwner } from "./state.ts";
import {ConversationAnnouncements, generationRejectionText} from './announcements.js';

type ConversationTab = 'conversation' | 'activity';
function useConversationTab(session: string | undefined) {
  const identity = useSyncExternalStore(presentationIdentity.subscribe, presentationIdentity.getSnapshot);
  const key = useMemo(() => identity && session ? 'rsi.trajectory.' + JSON.stringify([identity,session]) : undefined, [identity,session]);
  function load(): ConversationTab {
    try { return key && localStorage.getItem(key) === '1' ? 'activity' : 'conversation'; }
    catch { return 'conversation'; }
  }
  const [selection,setSelection] = useState(() => ({key,tab:load()}));
  if (selection.key !== key) setSelection({key,tab:load()});
  function selectTab(tab: ConversationTab) {
    if (tab === selection.tab) return;
    setSelection({key,tab});
    if (key) {
      try {
        if (tab === 'activity') localStorage.setItem(key, '1');
        else localStorage.removeItem(key);
      } catch { /* The open tab still works when device storage is unavailable. */ }
    }
  }
  return [selection.tab,selectTab] as const;
}

function NativeSurface({
  owner,
  selected,
  mode,
  column,
  catalogOpen,
}: {
  owner: SessionSurfaceOwner;
  selected: boolean;
  mode: string;
  column: HTMLElement | null;
  catalogOpen?: boolean;
}) {
  useSyncExternalStore(owner.subscribe, owner.getSnapshot);
  const data = owner.staged?.data;
  const empty = !data?.transcript.blocks.length;
  const earlier = !!data?.history_more || (!data?.historical && !!data?.transcript.omitted);
  const [tab, setTab] = useConversationTab(data?.session);
  const [announcements] = useState(() => new ConversationAnnouncements());
  const [announcement, announce] = useState<{revision: number; text: string}>();
  useLayoutEffect(() => {
    const changedSession = data && announcements.session !== data.session;
    const next = announcements.update(data, selected);
    if (next !== undefined) announce(next ?? undefined);
    else if (!selected || changedSession) announce(undefined);
  }, [announcements, data, selected]);
  const tabId = useId();
  const coordinates = data
    ? `${data.workspace.coordinates.path} · ${data.session}`
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
    <DocumentSessionScope view={data} owner={owner}>
    <section
      ref={ref("node")}
      className={`${css.scope} ${chat.scope} pane${selected ? " selected" : ""}${empty ? " is-empty" : ""}`}
      data-session-id={data?.session ?? ""}
      data-session-generation={data?.generation ?? ""}
      hidden={!selected}
      aria-label={`${label} conversation`}
    >
      <div role="status" aria-live="polite" aria-atomic="true" className="sr-only">
        {announcement && <span key={announcement.revision}>{announcement.text}</span>}
      </div>
      <header className="pane-header" hidden={empty}>
        <div className="pane-heading">
          <span className="pane-status" hidden>{data?.historical ? "History" : data?.transcript.status || "Ready"}</span>
          <nav className="transcript-tabs" role="tablist" aria-label="Conversation view" hidden={empty}>
            {(['conversation','activity'] as const).map(value=><button key={value} type="button" role="tab" id={`${tabId}-${value}`} aria-controls={`${tabId}-panel`} aria-selected={tab===value} tabIndex={tab===value?0:-1} onClick={()=>setTab(value)} onKeyDown={event=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(event.key)){event.preventDefault();const next=event.key==='Home'?'conversation':event.key==='End'?'activity':tab==='conversation'?'activity':'conversation';setTab(next);document.getElementById(`${tabId}-${next}`)?.focus();}}}>{value==='conversation'?'Chat':'Trajectory'}</button>)}
          </nav>
          {!empty && data?.agent_preset && <PresetLabel id={data.agent_preset}/>}
          {selected && !empty && data && <SessionPanel key={`panel:${data.session}`} path={data.workspace.coordinates.path} local={data.workspace.coordinates.location.kind === 'local'} chat={tab === 'conversation'}/>}
          {selected && !empty && data && <ScheduledTasks key={data.generation} pane={owner.index} surface={data}/>}
          <details className="session-menu">
            <summary aria-label="Session tools">
              <IconEllipsisOutlineRegular />
            </summary>
            <div className="pane-tools">
              <p className="pane-session mono">{coordinates}</p>
              {!empty && !data?.protected && <button type="button" className="quiet" disabled={owner.switching}
                onClick={event => {
                  closeDisclosure(event.currentTarget, true);
                  void run(() => owner.exportSession(''));
                }}>{owner.composerView?.exporting ? 'Cancel export' : 'Download conversation'}</button>}
              {control(
                "history",
                "Earlier history",
                "quiet",
                earlier,
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
          suspended={catalogOpen}
            pane={data?.protected ? undefined : owner.index}
          register={ref("transcript")}
          owner={owner}
          transcript={owner.staged?.data?.transcript ?? { blocks: [] }}
          generation={owner.staged?.data?.generation}
          cards={!!owner.staged?.data?.ui_cards}
          inline={data?.inline}
          mode={mode}
          trajectory={tab === "activity"}
          earlier={earlier}
        />
        <button ref={ref("bottom")} type="button" className={`back-bottom ${chat.bottom}`}
          onClick={() => elements.current.transcript.focus({preventScroll: true})}
          aria-label="Back to bottom" title="Back to bottom" hidden={tab !== 'conversation' || !owner.readingSummary}>
          <IconChevronDownOutlineRegular size={16}/>
        </button>
        {selected && <ConversationWidthControls container={column} showHandles={!empty && tab === 'conversation'} />}
      </div>
      {!data?.protected && !!data?.queue?.length && (
        <PendingInputs key={data.session} items={data.queue} owner={owner} editor={owner.queueDialog} />
      )}
      <PendingInteractions data={data} owner={owner} />
      <div className="pane-notice">{data?.notice}</div>
      {data?.projections?.generation_rebuilding && <p className="pane-notice" role="status">Updating plugins: rebuilding conversation context…</p>}
      {data?.projections?.generation_rejection && <p className="pane-notice">{generationRejectionText(data.projections.generation_rejection)}</p>}
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
      {owner.queueDialog && !owner.queueDialog.inlineSlot && (
        <QueueEditor key={`queue:${owner.queueDialog.id}`} state={owner.queueDialog} />
      )}
      <div className="composer-context" hidden={!empty}>
        <WorkspaceChoice owner={owner} attached={data?.workspace} workspace={owner.composerView?.workspace}/>
        {empty && (data && (data.draft === false || data.historical || data.protected) ? <PresetLabel id={data.agent_preset}/> : <PresetChoice owner={owner} applied={data?.agent_preset}/>)}
      </div>
      {data && !data.protected && !catalogOpen && <TodoPanel key={`todo:${data.session}`} projections={data.projections}/>}
      <Composer
        owner={owner}
        data={data}
        empty={empty}
        submitKey={owner.staged?.submitKey ?? "enter"}
        register={ref}
        hintId={hintId}
      />
      <div className="session-metrics-seat">
        {data && <SessionMetrics key={`metrics:${data.generation}`} owner={owner}/>}
      </div>
      <DocumentContributions seat="panel"/>
      <DocumentReviews/>
    </section>
    </DocumentSessionScope>
  );
}

export function SessionSurfaces({
  mode,
  column,
  catalogOpen,
}: {
  mode: string;
  column: HTMLElement | null;
  catalogOpen?: boolean;
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
            catalogOpen={catalogOpen}
          />
        ),
      )}
    </>
  );
}
