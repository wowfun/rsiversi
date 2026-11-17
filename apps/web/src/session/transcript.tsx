import {DocumentContributions, DocumentTranscript} from "../document-plugins.tsx";
import {IconCopyOutlineRegular} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";
import {writeClipboard} from "../../mounts.js";
import {messageClock, elapsedLabel} from '../../message-metrics.js';
import {MessageUsage, type TokenUsage} from './metrics.tsx';
import {ProcessTitle, RunningStatus} from './process-row.tsx';
import {
  PureComponent,
  Fragment,
  memo,
  useLayoutEffect,
  useRef,
  useState,
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
import { closeDisclosure } from "../../disclosure.js";
import { Markdown } from "./markdown.tsx";
import {ForkTurn} from './fork.tsx';

import type {TranscriptBlock, TranscriptView} from '../presentation-types.ts';
export interface MessageHost {
  node: HTMLElement;
  inline: HTMLElement;
  text: HTMLElement;
  sourceKey: string;
}
export interface TranscriptOwner {
  blocks: Map<string, MessageHost>;
  readingSummary?: boolean;
  setReadingSummary(value: boolean): void;
  action(action: string, fields?: Record<string, unknown>): Promise<unknown>;
}
export interface InlineCard {
  model?: unknown;
  ticket?: string;
  error?: string;
}
interface Props {
  inline?: Record<string, InlineCard>;
  owner: TranscriptOwner;
  transcript: TranscriptView;
  generation?: string;
  mode: string;
  cards: boolean;
  register: (node: HTMLElement | null) => void;
}
interface State {
  generation?: string;
  expanded: Set<string>;
}
type Position = ReturnType<typeof readingPosition>;

const Message = memo(function Message({
  block,
  owner,
  mode,
  cards,
  hidden,
  expanded,
  card,
  streaming,
  timestamp,
  usage,
}: {
  block: TranscriptBlock;
  owner: TranscriptOwner;
  mode: string;
  cards: boolean;
  hidden: boolean;
  expanded: boolean;
  card?: InlineCard;
  streaming: boolean;
  timestamp?: number;
  usage?: TokenUsage | null;
}) {
  const clock = messageClock(timestamp);
  const time = clock && <time className="message-clock" dateTime={clock.iso} title={clock.title}>{clock.label}</time>;
  const article = useRef<HTMLElement>(null),
    inline = useRef<HTMLDivElement>(null),
    body = useRef<HTMLDivElement>(null),
    menu = useRef<HTMLDetailsElement>(null);
  const [summary, setSummary] = useState(false);
  useLayoutEffect(() => {
    const transcript = article.current?.closest<HTMLElement>('.transcript');
    if (transcript && owner.readingSummary !== awayFromBottom(transcript)) owner.setReadingSummary(awayFromBottom(transcript));
  }, [summary]);
  const [copiedText, setCopied] = useState<string>();
  const copied = copiedText === block.text;
  const sourceKey = JSON.stringify([
    block.tool,
    block.external,
    block.sources,
    cards,
  ]);
  useLayoutEffect(() => {
    const entry = {
      node: article.current!,
      inline: inline.current!,
      text: body.current ?? article.current!,
      sourceKey,
    };
    owner.blocks.set(block.key, entry);
    return () => {
      if (owner.blocks.get(block.key) === entry) owner.blocks.delete(block.key);
    };
  }, [owner, block.key, sourceKey]);
  useLayoutEffect(() => {
    if (menu.current) menu.current.open = mode === "verbose";
  }, [mode]);
  const action = (
    label: string,
    name: string,
    fields: Record<string, unknown>,
  ) => (
    <button
      type="button"
      className={name === "delegation_open" ? "quiet delegation-action" : "quiet"}
      onClick={(event) => {
        closeDisclosure(event.currentTarget);
        void run(() => owner.action(name, fields));
      }}
    >
      {label}
    </button>
  );
  const foldable = block.role === "tool" || block.role === "reasoning";
  const policy = presentationPolicy(mode);
  const preview = streaming ? policy.liveDetail : block.role !== 'reasoning' || policy.reasoningPreview;
  return (
    <article
      ref={article}
      className={`message ${block.role}${block.external ? " delegation" : ""}${expanded ? " turn-expanded" : ""}${summary ? " summary-expanded" : ""}`}
      hidden={hidden && !block.external}
    >
      <DocumentTranscript block={block}>{foldable ? (
         <ProcessTitle block={block} open={summary || expanded} flat={expanded} preview={preview} toggle={() => setSummary(value => !value)}/>
      ) : (
        <p className="message-title">{block.title}</p>
      )}
      <div
        ref={body}
        className={`message-text${block.markdown ? " markdown" : ""}`}
      >
        {block.markdown ? <Markdown events={block.markdown} streaming={streaming} /> : block.text}
      </div></DocumentTranscript>
      <p className="omitted" hidden={!block.clipped}>
        Text shortened in this view.
      </p>
      {card && (!card.model || !card.ticket) && (
        <p className="inline-status" role="status">
          {card.error ?? "Loading card…"}
        </p>
      )}
      <div
        ref={inline}
        className="inline-card"
        hidden={!card?.model || !card.ticket}
      />
      {!!block.external &&
        action("Open external conversation", "delegation_open", {
          key: block.key,
        })}
      <div className="message-actions">
        {block.role === 'user' && time}
        <button type="button" hidden={!block.text} aria-label={copied ? "Copied message" : "Copy message"} title={copied ? "Copied" : "Copy"} onClick={()=>void run(async()=>{await writeClipboard(block.text);setCopied(block.text);})}><IconCopyOutlineRegular size={16}/></button>
      <details ref={menu} className="source-menu" hidden={!cards && !block.sources && !block.tool}>
        <summary aria-label="Message options">···</summary>
        <div className="actions source-actions">
          {cards && action("Card details", "ui_block", { key: block.key })}
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
        </div>
      </details>
      {block.role === 'assistant' && usage && <MessageUsage usage={usage}/>}
      {block.role !== 'user' && time}
      </div>
    </article>
  );
});

// React's before-mutation snapshot preserves the reader's anchor across message
// updates and turn folding. Nodes are keyed by attachment + block, never frame.
export class Transcript extends PureComponent<Props, State, Position> {
  state: State = { generation: this.props.generation, expanded: new Set() };
  container: HTMLDivElement | null = null;
  preserveAnchor = false;
  static getDerivedStateFromProps(props: Props, state: State) {
    return props.generation !== state.generation
      ? { generation: props.generation, expanded: new Set() }
      : null;
  }
  capture = (node: HTMLDivElement | null) => {
    this.container = node;
    this.props.register(node);
  };
  getSnapshotBeforeUpdate() {
    return readingPosition(this.container!, this.props.owner.readingSummary || this.preserveAnchor);
  }
  componentDidUpdate(previous: Props, _state: State, position: Position) {
    const container = this.container!;
    if (
      previous.generation !== this.props.generation ||
      (position.atEnd && container.scrollHeight !== position.previousHeight)
    )
      container.scrollTop = container.scrollHeight;
    else if (!position.atEnd) restoreAnchor(container, position.anchors);
    this.preserveAnchor = false;
    const away = awayFromBottom(container);
    if (this.props.owner.readingSummary !== away) this.props.owner.setReadingSummary(away);
  }
  toggle = (id: string) => {
    this.preserveAnchor = true;
    this.setState((state) => {
      const expanded = new Set(state.expanded);
      if (expanded.has(id)) expanded.delete(id);
      else expanded.add(id);
      return { expanded };
    });
  };
  render() {
    const { owner, transcript, mode, cards, generation } = this.props;
    const visibility = new Map<
        string,
        { hidden: boolean; expanded: boolean; streaming: boolean; timestamp?: number; usage?: TokenUsage | null }
      >(),
      summaries = new Map<string, ReactNode>();
    const completed = new Map<string,string>();
    const keys = new Set(transcript.blocks.map((block) => block.key));
    const replies = new Set(transcript.blocks.filter(block=>block.role === 'assistant').map(block=>block.key));
    const processes = new Set(transcript.blocks.filter(block=>block.role === 'tool' || block.role === 'reasoning').map(block=>block.key));
    for (const turn of Object.values(transcript.turns?.entries ?? {})) {
      const process = turn.process.filter(key=>processes.has(key));
      const Summary = process.length ? 'button' : 'div';
      if (turn.terminal && !turn.running) completed.set(turn.terminal, turn.id);
      const state = turnVisibility(
        turn,
        mode,
        this.state.expanded.has(turn.id),
        replies,
      );
      for (const key of turn.blocks)
        visibility.set(key, {
          hidden: state.hidden.has(key),
          expanded: state.expanded,
          streaming: turn.running,
          timestamp: turn.timestamps?.[key],
          usage: (turn.answer.at(-1) ?? turn.candidate.at(-1)) === key ? turn.usage : undefined,
        });
      const first = turn.process.find((key) => keys.has(key)) ?? (turn.terminal && turn.foldable && !turn.running ? turn.answer.find(key=>keys.has(key)) : undefined);
      if (first && (!turn.running || mode === 'compact') && mode !== "verbose")
        summaries.set(
          first,
          <Summary
            key={`turn:${turn.id}`}
            type={process.length ? "button" : undefined}
            className="turn-summary"
            data-turn-id={turn.id}
            aria-expanded={process.length ? !process.some(key=>state.hidden.has(key)) : undefined}
            onClick={process.length ? () => this.toggle(turn.id) : undefined}
          >
            {turn.running ? "Working" : turn.status}
            {!turn.running && turn.elapsed_ms != null && ` in ${elapsedLabel(turn.elapsed_ms)}`}
            {process.length > 0 && <> · {process.length} process {process.length === 1 ? "item" : "items"} {process.some(key=>state.hidden.has(key)) ? "⌄" : "⌃"}</>}
            {turn.partial ? " · Partial history" : ""}
          </Summary>,
        );
    }
    const nodes: ReactNode[] = [];
    if (transcript.omitted)
      nodes.push(
        <p key="omitted" className="omitted">
          Older content was omitted from this view. Open history to read earlier
          facts.
        </p>,
      );
    const runningTail = transcript.blocks.filter(item=>visibility.get(item.key)?.streaming).at(-1)?.key;
    for (const block of transcript.blocks) {
      if (summaries.has(block.key)) nodes.push(summaries.get(block.key));
      const state = visibility.get(block.key);
      nodes.push(
        <Message
          key={`${generation}:${block.key}`}
          block={block}
          owner={owner}
          mode={mode}
          cards={cards}
          card={this.props.inline?.[block.key]}
          hidden={(state?.hidden ?? false) || (block.role === 'metadata' && !!state && !state.expanded && mode !== 'verbose')}
          expanded={state?.expanded ?? false}
          streaming={state?.streaming ?? false}
          timestamp={state?.timestamp}
          usage={state?.usage}
        />,
      );
      if (block.key === runningTail) nodes.push(<RunningStatus key={`running:${generation}`}/>);
      const terminal = completed.get(block.key);
      if (terminal) nodes.push(<Fragment key={`tail:${generation}:${terminal}`}><DocumentContributions seat="turnTail" turn={terminal}/><ForkTurn turn={terminal} owner={owner}/></Fragment>);
    }
    return (
      <div
        ref={this.capture}
        className={`transcript${mode === "verbose" ? " mode-verbose" : ""}`}
        role="region"
        aria-label="Conversation transcript"
        tabIndex={0}
      >
        {nodes}
      </div>
    );
  }
}
