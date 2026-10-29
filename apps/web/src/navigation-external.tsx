import {useState} from 'react';
import {input, run, useView} from './bridge.ts';
import {Button} from './button.tsx';
import css from './navigation.module.css';

export function ExternalNavigation() {
  const external = useView(value => value?.external_catalog);
  const [pending, setPending] = useState<Record<string, string>>({});
  if (!external || (!external.endpoints.length && !external.conversations.length)) return null;
  const start = async (endpoint: string) => {
    const id = pending[endpoint] ?? `external_${crypto.randomUUID()}`;
    setPending(value => ({...value, [endpoint]: id}));
    await input.open({action:'external_start', id, endpoint});
    setPending(value => {const next = {...value}; delete next[endpoint]; return next;});
  };
  return <details className={`external-navigation ${css.external}`}>
    <summary>External agents</summary>
    <section aria-label="External agents">
      <Button size="sm" aria-label="Refresh external conversations" onClick={() => run(() => input.command({action:'external_catalog',next:false}))}>Refresh</Button>
      {external.endpoints.filter(endpoint => endpoint.enabled).map(endpoint =>
        <Button key={endpoint.id} size="sm" onClick={() => run(() => start(endpoint.id))}>{pending[endpoint.id] ? 'Check start' : 'Start'} {endpoint.id}</Button>)}
      {!external.endpoints.some(endpoint => endpoint.enabled) && <p className="hint">No external agents enabled in this Host Profile.</p>}
      {external.conversations.map(item => <button className="nav-item" key={item.id} title={item.id} onClick={() => run(() => input.open({action:'external_open',id:item.id}))}>
        <strong>{item.endpoint}</strong><small>{item.cwd.split(/[\\/]/).filter(Boolean).at(-1) ?? item.cwd}</small>
      </button>)}
      {external.more && <Button size="sm" onClick={() => run(() => input.command({action:'external_catalog',next:true}))}>More external conversations</Button>}
    </section>
  </details>;
}
