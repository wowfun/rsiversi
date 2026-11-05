import {useState} from 'react';
import {run} from '../bridge.ts';
import type {ForkSelection} from '../presentation-types.ts';
import type {TranscriptOwner} from './transcript.tsx';

export function ForkTurn({turn, owner}: {turn:string;owner:TranscriptOwner}) {
  const [mode, setMode] = useState('all');
  const [count, setCount] = useState('1');
  const [busy, setBusy] = useState(false);
  const number = Number(count);
  const valid = mode !== 'last' || (Number.isSafeInteger(number) && number > 0);
  const fork = async () => {
    if (busy || !valid) return;
    setBusy(true);
    try {
      const turns:ForkSelection = mode === 'last' ? {last:number} : mode === 'none' ? 'none' : 'all';
      await owner.action('fork_conversation', {turn, turns});
    } finally { setBusy(false); }
  };
  return <details className="source-menu fork-menu">
    <summary>Fork conversation</summary>
    <div className="actions source-actions">
      <label>History <select value={mode} disabled={busy} onChange={event=>setMode(event.target.value)}>
        <option value="all">Full history</option><option value="last">Recent turns</option><option value="none">No history</option>
      </select></label>
      {mode === 'last' && <label>Turns <input type="number" min="1" step="1" value={count} disabled={busy} onChange={event=>setCount(event.target.value)}/></label>}
      <button type="button" disabled={busy || !valid} onClick={()=>void run(fork)}>{busy ? 'Preparing…' : 'Create draft'}</button>
    </div>
  </details>;
}
