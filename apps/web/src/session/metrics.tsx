import {useCallback, useEffect, useState} from 'react';
import {IconDatabaseOutlineRegular, IconGaugeOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {tokenCount, tokenTotal, usageReadings, durationReading} from '../../message-metrics.js';
import type {SessionSurfaceOwner} from './state.ts';
import {StatPopover} from './stat-popover.tsx';
import css from './metrics.module.css';

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  reasoning_tokens: number | null;
}
interface SessionTiming {
  turns: number;
  timed_turns: number;
  total_ms: number;
  timed_attempts: number;
  model_ms: number;
  first_token_attempts: number;
  first_token_ms: number;
  decode_ms: number;
  decode_output_tokens: number;
}
export interface MetricsRead {
  complete: boolean;
  watermark: number;
  summary: {
    through_seq: number;
    attempts: number;
    reported_attempts: number;
    tokens: TokenUsage;
    timing: SessionTiming;
    last_context: {input_tokens: number} | null;
  };
}
const exactTokens = (value: number) => `${value.toLocaleString()} tok`;

function TokenDetails({usage}: {usage: TokenUsage}) {
  const cache = usage.input_tokens > 0 && usage.cache_read_tokens != null
    ? Math.round(100 * usage.cache_read_tokens / usage.input_tokens) : undefined;
  return <dl className={css.details}>
    {cache != null && <><dt>Cache hit</dt><dd>{cache}%</dd></>}
    <dt>{usage.cache_read_tokens == null ? 'Input' : 'Uncached input'}</dt>
    <dd>{exactTokens(usage.input_tokens - (usage.cache_read_tokens ?? 0))}</dd>
    {usage.cache_read_tokens != null && <><dt>Cached input</dt><dd>{exactTokens(usage.cache_read_tokens)}</dd></>}
    <dt>Output</dt><dd>{exactTokens(usage.output_tokens)}</dd>
    {!!usage.cache_write_tokens && <><dt title="Included in uncached input">Cache write</dt><dd>{exactTokens(usage.cache_write_tokens)}</dd></>}
    {!!usage.reasoning_tokens && <><dt title="Included in output">Reasoning</dt><dd>{exactTokens(usage.reasoning_tokens)}</dd></>}
  </dl>;
}
export function MessageUsage({usage}: {usage: TokenUsage}) {
  return <span className="message-usage" title="Reported usage for this model response">
    <StatPopover name="Token usage" icon={<IconDatabaseOutlineRegular/>}
      label={`Usage ${tokenCount(tokenTotal(usage))} tok`} headline={exactTokens(tokenTotal(usage)!)}>
      <TokenDetails usage={usage}/>
    </StatPopover>
  </span>;
}
export function SessionMetrics({owner}: {owner: SessionSurfaceOwner}) {
  const data = owner.staged?.data;
  const generation = data?.generation;
  const turns = Object.values(data?.transcript.turns?.entries ?? {});
  const settlement = turns.map(turn => turn.terminal ?? '').join('|');
  const [failure, setFailure] = useState('');
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    if (!generation) return;
    setBusy(true);
    try { await owner.action('metrics', {generation}); setFailure(''); }
    catch (error) { setFailure(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  }, [owner, generation]);
  useEffect(() => {if (settlement.replaceAll('|', '')) void refresh();}, [refresh, settlement]);
  const metrics = data?.metrics;
  if (!metrics && !failure) return null;
  const summary = metrics?.summary;
  const timing = summary?.timing;
  const readings = usageReadings(summary, data?.context_capacity_tokens);
  const counts = timing && summary ? `${timing.turns} ${timing.turns===1?'turn':'turns'} ${summary.attempts} ${summary.attempts===1?'step':'steps'}` : '';
  const retry = <button className={css.retry} type="button" disabled={busy} onClick={()=>void refresh()}>{busy?'Refreshing…':'Refresh statistics'}</button>;
  return <div className={css.footer} data-testid="session-metrics">
    <StatPopover name="Session statistics" icon={<IconGaugeOutlineRegular/>} headline={counts || undefined}
      label={failure ? 'Statistics unavailable' : `${counts}${readings.speed != null ? ` · ${Math.round(readings.speed)} tok/s` : ''}`}>
      <dl className={css.details}>
        {!!timing?.timed_attempts && <><dt>LLM time</dt><dd>{durationReading(timing.model_ms)}</dd></>}
        {!!timing?.timed_turns && <><dt title="Recorded Turn wall time, including waits">Total time</dt><dd>{durationReading(timing.total_ms)}</dd></>}
        {!!timing?.first_token_attempts && <><dt>Avg time to first token (TTFT)</dt><dd>{durationReading(timing.first_token_ms/timing.first_token_attempts)}</dd></>}
        {readings.speed != null && <><dt>Tokens per second (TPS)</dt><dd>{Math.round(readings.speed)} tok/s</dd></>}
        {!timing?.timed_attempts && summary && <><dt>Steps</dt><dd>{summary.attempts}</dd></>}
      </dl>
      {failure && <p role="status">{failure}</p>}
      {(failure || metrics && !metrics.complete) && retry}
    </StatPopover>
    {summary && readings.total != null && <StatPopover name="Token usage" icon={<IconDatabaseOutlineRegular/>}
      label={`${tokenCount(readings.total)} tok${readings.cache != null ? ` · Cache hit ${Math.round(readings.cache)}%` : ''}`}
      headline={exactTokens(readings.total)}>
      <TokenDetails usage={summary.tokens}/>
      {summary.reported_attempts < summary.attempts && <p>{summary.reported_attempts} of {summary.attempts} requests reported usage.</p>}
    </StatPopover>}
    {readings.context != null && summary?.last_context && data?.context_capacity_tokens && <StatPopover name="Context usage" compact
      icon={<span className={css.ring} aria-hidden="true"/>} label={`${Math.round(readings.context)}%`}>
      <div className={css.contextHeading} title="Last reported input and captured input capacity">
        <span><strong>{Math.round(readings.context)}%</strong> of context used</span>
        <strong>{tokenCount(summary.last_context.input_tokens)} / {tokenCount(data.context_capacity_tokens)}</strong>
      </div>
      <progress className={css.progress} aria-label="Context usage" max={data.context_capacity_tokens} value={summary.last_context.input_tokens}/>
      <dl className={css.details}>
        <dt><span className={css.dot}/>Input</dt><dd>{exactTokens(summary.last_context.input_tokens)}</dd>
        <dt><span className={`${css.dot} ${css.remaining}`}/>Available</dt><dd>{exactTokens(Math.max(0, data.context_capacity_tokens-summary.last_context.input_tokens))}</dd>
      </dl>
    </StatPopover>}
    {metrics && !metrics.complete && <span>Partial statistics</span>}
  </div>;
}
