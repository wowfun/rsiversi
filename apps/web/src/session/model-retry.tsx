import {useEffect, useState} from 'react';
import type {ModelRetry} from '../presentation-types.ts';
import css from './model-retry.module.css';

const labels = {
  scheduled: 'Retrying model request',
  started: 'Retried model request',
  cancelled: 'Model request retry cancelled',
};

function Countdown({delay}: {delay: number}) {
  const seconds = Math.max(1, Math.ceil(delay / 1000));
  const [remaining, setRemaining] = useState(seconds);
  useEffect(() => {
    if (seconds === 1) return;
    const deadline = performance.now() + delay;
    let timer: number;
    function tick() {
      const left = deadline - performance.now();
      const next = Math.max(1, Math.ceil(left / 1000));
      setRemaining(next);
      if (next > 1) timer = window.setTimeout(tick, left - (next - 1) * 1000);
    }
    tick();
    return () => window.clearTimeout(timer);
  }, [delay, seconds]);
  return <>{remaining}s</>;
}

export function ModelRetryRow({retry}: {retry: ModelRetry}) {
  return <details className={css.row} data-active={retry.state === 'scheduled' || undefined}>
    <summary><span role="status">{labels[retry.state]} ({retry.number}{retry.maximum != null && `/${retry.maximum}`}) · {retry.state === 'scheduled'
      ? <Countdown key={retry.number} delay={retry.delay_ms}/>
      : `${Math.max(1, Math.ceil(retry.delay_ms / 1000))}s`}</span></summary>
    <div className={css.details}><div>Retry delay: {retry.delay_ms} ms</div>
      <div>Failure reason: {retry.explanation} <code>{retry.reason}</code></div></div>
  </details>;
}
