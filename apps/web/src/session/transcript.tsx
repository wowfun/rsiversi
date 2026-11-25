import {DocumentContributions, DocumentTranscript} from "../document-plugins.tsx";
import {IconApiOutlineRegular, IconClockOutlineRegular, IconContextInjectionOutlineRegular, IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconGoalOutlineRegular, IconPaperPlaneOutlineRegular, IconUsersOutlineRegular} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import {MessageFeedback} from './message-feedback.tsx';
import {CopyButton} from "./copy-button.tsx";
import {TranscriptFlow} from './quote-action.tsx';
import {MessageImages} from './message-images.tsx';
import type {ImageInterest} from '../details/model.ts';
import type {Media} from './composer-state.ts';
import {messageClock, turnDurationLabel} from '../../message-metrics.js';
import {TurnUsage} from './metrics.tsx';
import {ProcessTitle, RunningStatus} from './process-row.tsx';
import {
  PureComponent,
  Fragment,
  memo,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import {
  readingPosition,
  restoreAnchor,
  awayFromBottom,
  turnVisibility,
  presentationPolicy,
} from "../../turn-presentation.js";
import { run } from "../bridge.ts";
import {activeResourceDock} from '../presentation.tsx';
import { closeDisclosure } from "../../disclosure.js";
import { Markdown } from "./markdown.tsx";
import {ForkTurn} from './fork.tsx';
import {CreatedTaskCards} from './scheduled-tasks.tsx';
import {PresentedFiles} from './presented-files.tsx';
import {ModelRetryRow} from './model-retry.tsx';
import {Trajectory, type TrajectoryMemory} from './trajectory.tsx';

import type {TranscriptBlock, TranscriptTurn, TranscriptView} from '../presentation-types.ts';
export interface MessageHost {
  node: HTMLElement;
  inline: HTMLElement;
  sourceKey: string;
}
export interface TranscriptOwner {
  blocks: Map<string, MessageHost>;
  readingSummary?: boolean;
  setReadingSummary(value: boolean): void;
  insertQuote?(text: string): boolean;
  acquireTranscriptImage(media:Media, generation:string, key:string, index:number): ImageInterest;
  action(action: string, fields?: Record<string, unknown>): Promise<unknown>;
  renderTranscript?(transcript: TranscriptView): void;
}
export interface InlineCard {
  model?: unknown;
  ticket?: string;
  error?: string;
}
interface Props {
  suspended?: boolean;
  pane?: string;
  inline?: Record<string, InlineCard>;
  owner: TranscriptOwner;
  transcript: TranscriptView;
  generation?: string;
  mode: string;
  trajectory?: boolean;
  earlier?: boolean;
  cards: boolean;
  register: (node: HTMLElement | null) => void;
}
interface State {
  generation?: string;
  expanded: Set<string>;
  trajectory: TrajectoryMemory;
}
const triggerIcons = {
  schedule:IconClockOutlineRegular, goal:IconGoalOutlineRegular, agent:IconPaperPlaneOutlineRegular,
  completion:IconUsersOutlineRegular, request:IconContextInjectionOutlineRegular,
};
function TurnTrigger({block,kind,time,open,toggle}: {
  block:TranscriptBlock;kind:NonNullable<TranscriptBlock['trigger']>;time:ReactNode;open:boolean;toggle:()=>void;
}) {
  const bodyId = useId();
  const Icon = triggerIcons[kind];
  return <><button className="trigger-header" type="button" aria-expanded={open} aria-controls={open?bodyId:undefined} onClick={toggle}>
    <Icon size={14}/><span>{block.title}</span>{time}<IconChevronDownOutlineRegular size={12}/>
  </button>{open && <div id={bodyId} className="trigger-body">
    <p>This notification triggered this response.</p>
    <DocumentTranscript block={block}><div className="message-text">{block.text}</div></DocumentTranscript>
  </div>}</>;
}
type Position = ReturnType<typeof readingPosition>;

function EarlierHistory({owner}: {owner:TranscriptOwner}) {
  const [loading,setLoading]=useState(false);
  async function load(){
    setLoading(true);
    await run(()=>owner.action('history',{}));
    setLoading(false);
  }
  return <div className="history-edge"><button type="button" className="quiet" disabled={loading} onClick={()=>void load()}>
    {loading?'Loading earlier…':'Load earlier'}
  </button></div>;
}

function TranscriptRow({label,caption,icon,open,busy,hint,onClick,status}: {
  label:string; caption:string; icon:ReactNode; open?:boolean; busy?:boolean; hint?:string; onClick?:()=>void;
  status?:'running'|'settled';
}) {
  const content=<>
    <span className="compaction-icon" aria-hidden="true">{icon}
      {!status&&(open ? <IconChevronDownOutlineRegular/> : <IconChevronRightOutlineRegular/>)}</span>
    <span>{label}</span><span className="compaction-separator" aria-hidden="true"/>
    <span className="compaction-caption" data-shimmer={status==='running'||undefined}>{caption}</span>
    {hint&&<span className="sr-only">{hint}</span>}
  </>;
  if(status)return <div className="compaction-status" role={status==='running'?'status':undefined}>{content}</div>;
  return <button className="compaction-toggle" type="button" disabled={!onClick||busy}
    aria-expanded={onClick?open:undefined} aria-busy={busy} title={hint} onClick={onClick}>{content}</button>;
}

function CompactionRow({block,pane,generation}: {block:TranscriptBlock; pane?:string; generation?:string}) {
  return <article className="compaction-row">{block.summary_seq===undefined
    ? <TranscriptRow label="compact" caption={block.title} icon={<IconApiOutlineRegular/>}
        status={block.compacting?'running':'settled'}/>
    : <CompactionSummary caption={block.title} sequence={block.summary_seq} pane={pane} generation={generation}/>}
  </article>;
}
function CompactionSummary({caption,sequence,pane,generation}: {caption:string;sequence:string;pane?:string;generation?:string}) {
  const dock=useSyncExternalStore(activeResourceDock.subscribe,activeResourceDock.getSnapshot);
  const available=dock!==undefined&&dock.pane===pane&&dock.generation===generation;
  const pending=useRef<AbortController|null>(null);
  const [opening,setOpening]=useState(false);
  const [failure,setFailure]=useState('');
  useEffect(()=>{
    setOpening(false);
    setFailure('');
    return ()=>{pending.current?.abort();pending.current=null};
  },[sequence,dock]);
  async function open() {
    if(!available||pending.current)return;
    const request=new AbortController();
    pending.current=request;
    setOpening(true);
    setFailure('');
    try{
      await dock.openSummary(sequence,request.signal);
    }catch(error){
      if(!request.signal.aborted)setFailure(error instanceof Error?error.message:String(error));
    }finally{
      if(pending.current===request){pending.current=null;setOpening(false)}
    }
  }
  return <>
    <TranscriptRow label="compact" caption={caption} icon={<IconApiOutlineRegular/>}
      busy={opening} hint={available?'View compaction summary in the sidebar':undefined}
      onClick={available?()=>void open():undefined}/>
    {failure&&<p role="alert">{failure}</p>}
  </>;
}

function hasSources(block: TranscriptBlock, cards: boolean) {
  return !!(cards || block.sources || block.tool || block.request_intent_seq);
}

function SourceMenu({block, cards, mode, owner}: {
  block: TranscriptBlock; cards: boolean; mode: string; owner: TranscriptOwner;
}) {
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const action = (label:string, name:string, fields:Record<string,unknown>) =>
    <button type="button" className="quiet" onClick={event=>{
      closeDisclosure(event.currentTarget);
      void run(()=>owner.action(name,fields));
    }}>{label}</button>;
  return <details open={mode === 'verbose' || undefined} className="source-menu" onToggle={event=>setSourcesOpen(event.currentTarget.open)}>
    <summary aria-label="Message options">···</summary>
    {(sourcesOpen || mode === 'verbose') && <div className="actions source-actions">
      {cards && action("Card details", "ui_block", { key: block.key })}
      {block.request_intent_seq && <>
        {action("Tool names and changes", "inspect_source", {source: {seq: block.request_intent_seq, field: {kind: "model_tool_summary"}}})}
        {action("Full tool schemas and choice", "inspect_source", {source: {seq: block.request_intent_seq, field: {kind: "model_tool_schemas"}}})}
      </>}
      {block.sources > 0 &&
        action("Inspect sources", "inspect_block", { key: block.key })}
      {block.tool && (
        <>
          {(["arguments", "result", "rejection"] as const).map((field) =>
            block.tool?.[field] ? (
              <Fragment key={field}>
                {action(`Inspect ${field}`, "inspect_source", {
                  source: block.tool[field],
                })}
              </Fragment>
            ) : null,
          )}
          {!block.tool.intent_present &&
            block.tool.phase !== "rejected" && (
              <span className="hint">Intent not loaded</span>
            )}
        </>
      )}
    </div>}
  </details>;
}

const Message = memo(function Message({
  block,
  generation,
  owner,
  mode,
  cards,
  hidden,
  expanded,
  card,
  streaming,
  timestamp,
  turnAssistant,
  closingReply,
  revealActions,
}: {
  block: TranscriptBlock;
  generation?: string;
  owner: TranscriptOwner;
  mode: string;
  cards: boolean;
  hidden: boolean;
  expanded: boolean;
  card?: InlineCard;
  streaming: boolean;
  timestamp?: number;
  turnAssistant: boolean;
  closingReply: boolean;
  revealActions: boolean;
}) {
  const clock = messageClock(timestamp);
  const time = clock && <time className="message-clock" dateTime={clock.iso} title={clock.title}>{clock.label}</time>;
  const article = useRef<HTMLElement>(null),
    inline = useRef<HTMLDivElement>(null);
  const [summary, setSummary] = useState(false);
  const context = block.role === 'context';
  const foldable = block.role === "tool" || block.role === "reasoning";
  const showHeader = (!(context || block.trigger) || summary) && (!hidden || !!block.external);
  const showBody = showHeader && (!foldable || summary || expanded);
  const sources = !closingReply && hasSources(block, cards);
  useLayoutEffect(() => {
    const transcript = article.current?.closest<HTMLElement>('.transcript');
    if (transcript && owner.readingSummary !== awayFromBottom(transcript)) owner.setReadingSummary(awayFromBottom(transcript));
  }, [summary]);
  const sourceKey = cards ? JSON.stringify([
    block.tool,
    block.external,
    block.sources,
  ]) : '';
  const hasCardHost = cards || !!card;
  useLayoutEffect(() => {
    if (!hasCardHost) return;
    const entry = {
      node: article.current!,
      inline: inline.current!,
      sourceKey,
    };
    owner.blocks.set(block.key, entry);
    return () => {
      if (owner.blocks.get(block.key) === entry) owner.blocks.delete(block.key);
    };
  }, [owner, block.key, sourceKey, hasCardHost]);
  const policy = presentationPolicy(mode);
  const preview = streaming ? policy.liveDetail : block.role !== 'reasoning' || policy.reasoningPreview;
  return (
    <article
      ref={article}
      data-closing-reply={closingReply || undefined}
      data-actions-reveal={revealActions ? 'hover' : undefined}
      className={`message ${block.role}${block.trigger ? " turn-trigger" : ""}${block.external ? " delegation" : ""}${expanded ? " turn-expanded" : ""}${summary ? " summary-expanded" : ""}`}
      hidden={hidden && !block.external}
    >
      {context && <TranscriptRow label="Context injection" caption={block.title} icon={<IconContextInjectionOutlineRegular/>}
        open={summary} onClick={()=>setSummary(value=>!value)}/>}
      {block.trigger && <TurnTrigger block={block} kind={block.trigger} time={time} open={summary} toggle={()=>setSummary(value=>!value)}/>}
      {showHeader && !block.trigger && <DocumentTranscript block={block}>{foldable ? (
         <ProcessTitle block={block} open={summary || expanded} flat={expanded} preview={preview} running={streaming} toggle={() => setSummary(value => !value)}/>
      ) : !context && <p className="message-title">{block.title}</p>}
      {showBody && generation && block.images && <MessageImages owner={owner} generation={generation} messageKey={block.key} images={block.images}/>}
      {showBody && (!block.images || block.text) && <div
        className={`message-text${block.markdown ? " markdown" : ""}`}
        data-text-interaction-source={block.role === 'user' || (!streaming && (block.role === 'assistant' || block.role === 'reasoning')) ? '' : undefined}
      >
        {block.markdown ? <Markdown events={block.markdown} streaming={streaming} /> : block.text}
      </div>}</DocumentTranscript>}
      {showBody && block.clipped && <p className="omitted">
        Text shortened in this view.
      </p>}
      {card && (!card.model || !card.ticket) && (
        <p className="inline-status" role="status">
          {card.error ?? "Loading card…"}
        </p>
      )}
      {hasCardHost && <div
        ref={inline}
        className="inline-card"
        hidden={!card?.model || !card.ticket}
      />}
      {!!block.external && <button type="button" className="quiet delegation-action" onClick={event=>{
        closeDisclosure(event.currentTarget);
        void run(()=>owner.action('delegation_open',{key:block.key}));
      }}>Open external conversation</button>}
      {showBody && (sources || !turnAssistant && !!(time || block.text)) && <div className="message-actions">
        {block.role === 'user' && time}
        {!turnAssistant && block.text && <CopyButton text={block.text} label="Copy message"/>}
      {sources && <SourceMenu block={block} cards={cards} mode={mode} owner={owner}/>}
      {!turnAssistant && block.role !== 'user' && !block.trigger && time}
      </div>}
    </article>
  );
});

// React's before-mutation snapshot preserves the reader's anchor across message
// updates and turn folding. Nodes are keyed by attachment + block, never frame.
export class Transcript extends PureComponent<Props, State, Position | undefined> {
  state: State = { generation: this.props.generation, expanded: new Set(), trajectory:{} };
  container: HTMLDivElement | null = null;
  toggleHeading?: HTMLElement;
  chatPosition: number | undefined;
  suspendedPosition?: {generation?:string; top:number; atEnd:boolean};
  static getDerivedStateFromProps(props: Props, state: State) {
    return props.generation !== state.generation
      ? { generation: props.generation, expanded: new Set(), trajectory:{} }
      : null;
  }
  capture = (node: HTMLDivElement | null) => {
    this.container = node;
    this.props.register(node);
  };
  getSnapshotBeforeUpdate(previous: Props) {
    if (this.props.suspended) {
      if (!previous.suspended) {
        const element = this.container!;
        this.suspendedPosition = {generation:previous.generation,top:element.scrollTop,atEnd:!previous.trajectory && !awayFromBottom(element)};
      }
      return;
    }
    if (previous.suspended) return;
    if (!previous.trajectory && this.props.trajectory) this.chatPosition = this.container!.scrollTop;
    return readingPosition(this.container!, this.props.owner.readingSummary, this.toggleHeading);
  }
  componentDidUpdate(previous: Props, _state: State, position?: Position) {
    if (this.props.suspended) return;
    const container = this.container!;
    if (previous.suspended) {
      const saved = this.suspendedPosition;
      container.scrollTop = saved && saved.generation === this.props.generation && !saved.atEnd ? saved.top : container.scrollHeight;
      this.suspendedPosition = undefined;
    } else if (previous.trajectory !== this.props.trajectory) {
      this.props.owner.renderTranscript?.(this.props.transcript);
      container.scrollTop = this.props.trajectory ? 0 : this.chatPosition ?? container.scrollHeight;
    } else if (previous.generation !== this.props.generation || (!this.props.trajectory && position?.atEnd))
      container.scrollTop = container.scrollHeight;
    else if (position && !position.atEnd) restoreAnchor(container, position.anchors);
    this.toggleHeading = undefined;
    const away = awayFromBottom(container);
    if (this.props.owner.readingSummary !== away) this.props.owner.setReadingSummary(away);
  }
  toggle = (id: string, heading: HTMLElement) => {
    this.toggleHeading = heading;
    this.setState((state) => {
      const expanded = new Set(state.expanded);
      if (expanded.has(id)) expanded.delete(id);
      else expanded.add(id);
      return { expanded };
    });
  };
  render() {
    const { owner, transcript, mode, cards, generation, pane } = this.props;
    if (this.props.suspended) return <div ref={this.capture} className="transcript"/>;
    if (this.props.trajectory)
      return <div ref={this.capture} className="transcript trajectory" role="region" aria-label="Trajectory" tabIndex={0}>
        <Trajectory key={generation} owner={owner} transcript={transcript} earlier={!!this.props.earlier} memory={this.state.trajectory}/>
      </div>;
    const visibility = new Map<
        string,
        { hidden: boolean; expanded: boolean; streaming: boolean; closingReply:boolean; timestamp?: number }
      >(),
      summaries = new Map<string, ReactNode>();
    const completed = new Map<string,TranscriptTurn>();
    const blocks = new Map<string,TranscriptBlock>();
    const replies = new Set<string>();
    let hasCreatedTasks = false;
    let hasPresentedFiles = false;
    let latestUser: string | undefined, latestContent: string | undefined;
    for (const block of transcript.blocks) {
      blocks.set(block.key, block);
      if (block.role === 'assistant') replies.add(block.key);
      if (block.created_schedule) hasCreatedTasks = true;
      if (block.presented) hasPresentedFiles = true;
      if (block.role === 'user' && !block.trigger) latestUser = block.key;
      if (block.role !== 'status' && block.role !== 'metadata') latestContent = block.key;
    }
    for (const turn of Object.values(transcript.turns?.entries ?? {})) {
      if (turn.terminal && !turn.running) completed.set(turn.terminal, turn);
      const open = this.state.expanded.has(turn.id);
      const state = turnVisibility(
        turn,
        mode,
        open,
        replies,
      );
      const lastProse = turn.closing_prose?.at(-1);
      const closing = mode !== 'verbose' && turn.terminal && !turn.running && blocks.has(turn.terminal) && lastProse !== undefined
        ? turn.blocks[lastProse] : undefined;
      for (const key of turn.blocks)
        visibility.set(key, {
          hidden: state.hidden.has(key),
          expanded: state.expanded,
          streaming: turn.running,
          closingReply: key === closing,
          timestamp: turn.timestamps?.[key],
        });
      const first = turn.process.find(key => blocks.has(key))
        ?? (turn.terminal && turn.foldable && !turn.running ? turn.answer.find(key => blocks.has(key)) : undefined);
      if (first && (!turn.running || mode === 'compact') && mode !== "verbose") {
        const expandable = turn.foldable && turn.process.some(key => {
          const role = blocks.get(key)?.role;
          return role === 'tool' || role === 'reasoning' || role === 'context';
        });
        const Summary = expandable ? 'button' : 'div';
        const duration = !turn.running && turnDurationLabel(turn.status, turn.elapsed_ms);
        summaries.set(
          first,
          <Summary
            key={`turn:${turn.id}`}
            type={expandable ? "button" : undefined}
            className="turn-summary"
            data-turn-id={turn.id}
            aria-expanded={expandable ? open : undefined}
            onClick={expandable ? event => this.toggle(turn.id,event.currentTarget) : undefined}
          >
            <span>{turn.running ? "Working" : turn.status}{duration && ` in ${duration}`}{turn.partial && " · Partial history"}</span>
            {expandable && <IconChevronDownOutlineRegular size={14} aria-hidden="true"/>}
          </Summary>,
        );
      }
      for (const retry of turn.retries ?? []) {
        const key = turn.blocks[retry.block];
        summaries.set(key, <Fragment key={`retry:${turn.id}:${key}`}>
          {summaries.get(key)}
          <ModelRetryRow retry={retry}/>
        </Fragment>);
      }
    }
    const nodes: ReactNode[] = [];
    if (this.props.earlier)
      nodes.push(<EarlierHistory key={`earlier:${generation}`} owner={owner}/>);
    else if (transcript.omitted)
      nodes.push(
        <p key="omitted" className="omitted">
          Older content was omitted from this view. Open history to read earlier
          facts.
        </p>,
      );
    let runningTail: string | undefined;
    for (const block of transcript.blocks) {
      if (visibility.get(block.key)?.streaming) runningTail = block.key;
    }
    for (const block of transcript.blocks) {
      if (block.role === "compaction") {
        nodes.push(<CompactionRow key={`${generation}:${block.key}`} block={block} pane={pane} generation={generation}/>);
        continue;
      }
      if (summaries.has(block.key)) nodes.push(summaries.get(block.key));
      const state = visibility.get(block.key);
      const hidden = state?.hidden ?? false;
      if (block.role !== 'metadata' || !state || !hidden &&
        (mode === 'verbose' || state.expanded && !block.scheduled_retry)) nodes.push(
        <Message
          key={`${generation}:${block.key}`}
          block={block}
          generation={generation}
          owner={owner}
          mode={mode}
          cards={cards}
          card={this.props.inline?.[block.key]}
          hidden={hidden}
          expanded={state?.expanded ?? false}
          streaming={state?.streaming ?? false}
          timestamp={state?.timestamp}
          turnAssistant={!!state && block.role === 'assistant'}
          closingReply={state?.closingReply ?? false}
          revealActions={block.role === 'assistant' || (block.role === 'user' && !block.trigger && block.key !== latestUser)}
        />,
      );
      if (block.key === runningTail) nodes.push(<RunningStatus key={`running:${generation}`}/>);
      const terminal = completed.get(block.key);
      if (terminal) {
        const keys = terminal.closing_prose?.map(index => terminal.blocks[index]) ?? [];
        const text = keys.map(key => blocks.get(key)!.text);
        const last = keys.at(-1);
        const closing = last && visibility.get(last)?.closingReply ? blocks.get(last) : undefined;
        const clock = last && messageClock(terminal.timestamps?.[last]);
        let taskCards: ReactNode;
        if (hasCreatedTasks && pane && generation) {
          const created = terminal.process.flatMap(key => {
            const block = blocks.get(key);
            return block?.created_schedule && block.tool?.result
              ? [{key,task:block.created_schedule,seq:block.tool.result.seq}] : [];
          });
          created.sort((a,b)=>a.seq.length-b.seq.length || (a.seq < b.seq ? -1 : a.seq > b.seq ? 1 : 0));
          if (created.length) taskCards = <CreatedTaskCards rows={created} pane={pane} generation={generation}/>;
        }
        nodes.push(<div className="turn-tail" key={`tail:${generation}:${terminal.id}`} data-turn-tail={terminal.id}
          data-actions-reveal={last && last === latestContent && terminal.answer.includes(last) ? undefined : 'hover'}>
          <DocumentContributions seat="turnTail" turn={terminal.id}/>
          {hasPresentedFiles && pane && generation && <PresentedFiles process={terminal.process} blocks={blocks}
            pane={pane} generation={generation} owner={owner}/>}
          {taskCards}
          <div className="message-actions">
            {text.length > 0 && <CopyButton label="Copy message" text={text}/>}
            {pane && generation && terminal.feedback_message && <MessageFeedback key={terminal.feedback_message}
              pane={pane} generation={generation} message={terminal.feedback_message}/>}
            <ForkTurn turn={terminal.id} owner={owner}/>
            {terminal.usage && <TurnUsage usage={terminal.usage} routes={terminal.usage_routes}/>}
            {closing && hasSources(closing,cards) && <SourceMenu block={closing} cards={cards} mode={mode} owner={owner}/>}
            {clock && <time className="message-clock" dateTime={clock.iso} title={clock.title}>{clock.label}</time>}
          </div>
        </div>);
      }
    }
    return (
      <div
        ref={this.capture}
        className={`transcript${mode === "verbose" ? " mode-verbose" : ""}`}
        role="region"
        aria-label="Conversation transcript"
        tabIndex={0}
      >
        <TranscriptFlow insert={this.props.pane ? owner.insertQuote : undefined}>{nodes}</TranscriptFlow>
      </div>
    );
  }
}
