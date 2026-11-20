import { Fragment, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { TranscriptBlock, TranscriptTurn, TranscriptView } from '../presentation-types.ts';
import { IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconSearchOutlineRegular } from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import { elapsedLabel, messageClock } from '../../message-metrics.js';
import { run } from '../bridge.ts';
import { Markdown } from './markdown.tsx';
import type { TranscriptOwner } from './transcript.tsx';
import css from './trajectory.module.css';
import {TrajectoryTimeline} from '../../vendor/dsh/trajectory/TrajectoryTimeline.tsx';
import {trajectoryTimelineFocusIndexes, type TrajectoryTimeRange} from '../../vendor/dsh/trajectory/timeline.ts';
import {translateTimeline} from '../../vendor/dsh/trajectory/locales.ts';
import type {TrajectoryCellKind} from '../../vendor/dsh/trajectory/trajectory-record.ts';

interface Group {
  key: string;
  turn?: TranscriptTurn;
  records: TranscriptBlock[];
}

function groupsOf(transcript: TranscriptView): Group[] {
  const turns = new Map<string, TranscriptTurn>();
  for (const turn of Object.values(transcript.turns?.entries ?? {}))
    for (const key of turn.blocks) turns.set(key, turn);
  const groups: Group[] = [];
  let group: Group | undefined;
  for (const block of transcript.blocks) {
    const turn = turns.get(block.key);
    if (!group || turn !== group.turn) {
      group = { key: turn ? 'turn:'+turn.id : block.key, turn, records: [] };
      groups.push(group);
    }
    group.records.push(block);
  }
  return groups;
}

function kind(block: TranscriptBlock) {
  if (block.request_intent_seq) return 'Request';
  if (block.role === 'metadata') return block.title;
  return block.tool?.name ?? block.role;
}

// Patch frames retain unchanged block objects. Discard their normalized text
// when the corresponding history window leaves this view.
function matches(index: WeakMap<TranscriptBlock,string>, block: TranscriptBlock, terms: string[], group: string) {
  if (!terms.length) return true;
  let text = index.get(block);
  if (text === undefined) {
    text = [block.role, block.title, block.text, block.tool?.name, block.tool?.argument_summary]
      .join('\n').toLocaleLowerCase();
    index.set(block, text);
  }
  return terms.every(term => group.includes(term) || text.includes(term));
}

function Details({block, owner, close}: {block: TranscriptBlock; owner: TranscriptOwner; close(): void}) {
  const action = (name: string, fields: Record<string, unknown>) => void run(() => owner.action(name, fields));
  const source = (label: string, selector: unknown) =>
    <button type="button" key={label} onClick={() => action('inspect_source', {source:selector})}>{label}</button>;
  return <aside className={css.detail} aria-label="Event details">
    <header><strong>{block.title}</strong><button type="button" aria-label="Close details" onClick={close}>×</button></header>
    <div className={css.body}>
      {block.markdown ? <div className="markdown"><Markdown events={block.markdown}/></div> : <pre>{block.text}</pre>}
      {block.clipped && <p>Text shortened in this view.</p>}
      <div className={css.actions}>
        {block.request_intent_seq && <>
          {source('Tool names and changes', {seq:block.request_intent_seq,field:{kind:'model_tool_summary'}})}
          {source('Full tool schemas and choice', {seq:block.request_intent_seq,field:{kind:'model_tool_schemas'}})}
        </>}
        {(['arguments','result','rejection'] as const).map(field =>
          block.tool?.[field] ? source('Inspect '+field,block.tool[field]) : null)}
        {block.sources > 0 && <button type="button" onClick={() => action('inspect_block', {key:block.key})}>Inspect sources</button>}
        {!!block.external && <button type="button" onClick={() => action('delegation_open', {key:block.key})}>Open external conversation</button>}
      </div>
    </div>
  </aside>;
}

export interface TrajectoryMemory {
  query?: string; collapsed?: Set<string>; selection?: string; actual?: boolean;
  range?: TrajectoryTimeRange | null; scroll?: number;
}

function cellKind(block: TranscriptBlock): TrajectoryCellKind {
  if (block.tool || block.role === 'tool') return 'tool';
  if (block.role === 'assistant' || block.role === 'reasoning') return 'message';
  if (block.role === 'compaction') return 'compacted';
  if (block.role === 'user') return 'user';
  return block.role === 'system' ? 'system' : 'context';
}

export function Trajectory({transcript, owner, earlier, memory}: {transcript: TranscriptView; owner: TranscriptOwner; earlier: boolean; memory: TrajectoryMemory}) {
  const groups = useMemo(() => groupsOf(transcript), [transcript]);
  const [query, setQuery] = useState(memory.query ?? '');
  const [collapsed, setCollapsed] = useState(() => memory.collapsed ?? new Set<string>());
  const [selection, select] = useState(memory.selection ?? '');
  const [actual, setActual] = useState(memory.actual ?? false);
  const [range, setRange] = useState(memory.range ?? null);
  const mode = actual ? 'time' : 'sequence';
  const ledger = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    Object.assign(memory, {query, collapsed, selection, actual, range});
  }, [memory, query, collapsed, selection, actual, range]);
  useLayoutEffect(() => {
    const node = ledger.current!;
    node.scrollTop = memory.scroll ?? 0;
    return () => { memory.scroll = node.scrollTop; };
  }, [memory]);
  const index = useRef(new WeakMap<TranscriptBlock,string>());
  const buttons = useRef(new Map<string, HTMLButtonElement>());
  const records = useMemo(() => {
    const byKey = new Map<string,number>();
    const entries: {block:TranscriptBlock; group:Group}[] = [];
    let turnNumber = 0;
    const turns = groups.map(group => ({
      turn: group.turn ? ++turnNumber : null,
      groups: [{title:'', cells:group.records.map(block => {
        const i = entries.push({block, group});
        byKey.set(block.key, i);
        const phase = block.tool?.phase;
        return {index:i, kind:cellKind(block), text:block.title, startedAt:group.turn?.timestamps?.[block.key] ?? null,
          timeSeconds:null, isError:phase === 'rejected' || (typeof phase === 'object' && phase.settled !== 'completed')};
      })}],
    }));
    return {entries, byKey, turns};
  }, [groups]);
  const terms = useMemo(() => query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean), [query]);
  const focused = useMemo(() => range ? trajectoryTimelineFocusIndexes(records.turns,range,mode) : null, [records,range,mode]);
  const {labeled, found} = useMemo(() => {
    const found = terms.length ? new Set<number>() : null;
    const labeled = groups.map((group,i) => {
      const number = records.turns[i].turn;
      const label = number === null ? 'Between turns' : 'Turn '+number;
      const visible = group.records.filter(block => {
        const id = records.byKey.get(block.key)!;
        if (!matches(index.current, block, terms, label.toLowerCase())) return false;
        found?.add(id);
        return !focused || focused.has(id);
      });
      return {...group, label, visible};
    });
    return {labeled, found};
  }, [groups, records, terms, focused]);
  const turns = labeled.filter(group => group.turn && group.visible.length);
  const allFolded = turns.length > 0 && turns.every(group => collapsed.has(group.key));
  const selectedId = records.byKey.get(selection);
  const selected = selectedId && (!found || found.has(selectedId)) && (!focused || focused.has(selectedId))
    ? records.entries[selectedId-1].block : undefined;
  const toggle = (key: string) => setCollapsed(old => {
    const next = new Set(old);
    if (next.has(key)) next.delete(key); else next.add(key);
    return next;
  });
  const pick = (i: number, show: boolean) => {
    const {block,group} = records.entries[i-1];
    if (found && !found.has(i)) setQuery('');
    if (show) select(block.key);
    if (collapsed.has(group.key)) toggle(group.key);
    requestAnimationFrame(() => buttons.current.get(block.key)?.scrollIntoView({block:'nearest'}));
  };
  const close = () => { select(''); buttons.current.get(selection)?.focus({preventScroll:true}); };
  return <div className={css.root}>
    <div role="toolbar" aria-label="Trajectory toolbar" className={css.toolbar}>
      <button type="button" aria-label="Actual time" aria-pressed={actual} onClick={() => {setActual(!actual); setRange(null);}}
        disabled={!records.turns.some(turn => turn.groups[0].cells.some(cell => cell.startedAt !== null))}>Actual time</button>
      <button type="button" aria-label={allFolded?'Expand turns':'Collapse turns'} aria-pressed={allFolded}
        disabled={!turns.length} onClick={() => setCollapsed(allFolded ? new Set() : new Set(turns.map(group => group.key)))}>Turns</button>
      <label><IconSearchOutlineRegular size={14}/><input type="search" aria-label="Search trajectory" placeholder="Search" value={query} onChange={event => setQuery(event.target.value)}/></label>
    </div>
    <TrajectoryTimeline turns={records.turns} t={translateTimeline} mode={mode} range={range} onRangeChange={setRange}
      searchMatchIndexes={found} selectedIndex={selected ? selectedId : null} onRecordSelect={i => pick(i,true)}
      onRecordFocus={i => pick(i,false)} hasEarlierRecords={earlier} onLoadEarlier={() => run(() => owner.action('history'))}/>
    <div ref={ledger} className={css.ledger} data-trajectory-scroll="">
      <table aria-label="Trajectory records"><colgroup><col/><col/></colgroup><tbody>
        {labeled.filter(group => group.visible.length).map(group => <Fragment key={group.key}>
          <tr className={css.group}><th colSpan={2} scope="rowgroup">
            <button type="button" aria-expanded={!collapsed.has(group.key)} onClick={() => toggle(group.key)} title={group.turn?.id}>
              {collapsed.has(group.key) ? <IconChevronRightOutlineRegular size={12}/> : <IconChevronDownOutlineRegular size={12}/>}
              {group.label}
              {group.turn && <span>{group.turn.status}{group.turn.elapsed_ms != null && ' · '+elapsedLabel(group.turn.elapsed_ms)}</span>}
            </button>
          </th></tr>
          {!collapsed.has(group.key) && group.visible.map(block => {
            const time = messageClock(group.turn?.timestamps?.[block.key]);
            const preview = block.text || block.tool?.argument_summary || block.title;
            return <tr key={block.key} data-trajectory-row-key={block.key} data-selected={selection===block.key || undefined}>
              <td className={css.kind} title={time?.title}>{kind(block)}</td><td>
                <button type="button" className={css.record} ref={node => {if (node) buttons.current.set(block.key,node); else buttons.current.delete(block.key);}}
                  aria-expanded={selection===block.key} onClick={() => select(selection===block.key?'':block.key)}>
                  {preview.slice(0,320)}
                </button>
              </td>
            </tr>;
          })}
        </Fragment>)}
      </tbody></table>
      {terms.length > 0 && !labeled.some(group => group.visible.length > 0) && <p role="status">No matching records</p>}
    </div>
    {selected && <Details block={selected} owner={owner} close={close}/>}
  </div>;
}
