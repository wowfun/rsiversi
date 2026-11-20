import { useEffect, useState } from 'react';
import { input, useView } from '../bridge.ts';
import { presetName } from '../session/preset-choice.tsx';
import type { SetupActions } from './setup-actions.ts';
import css from './settings.module.css';

export function ConversationDefaults({ actions }: { actions: SetupActions }) {
  const setup = useView(view => view?.setup);
  const presets = useView(view => view?.catalog.presets?.presets);
  const [error, setError] = useState('');
  useEffect(() => {
    let current = true;
    void input.command({action:'presets'}).catch(cause => { if(current) setError(String(cause)); });
    return () => { current = false; };
  }, []);
  const selected = setup?.presets?.default ?? presets?.find(preset => preset.is_default)?.id;
  return <section className="setup-section">
    <h2>Conversation defaults</h2>
    <h3>Default preset</h3>
    {error && <p role="alert">{error}</p>}
    {!presets && !error && <p role="status">Loading presets…</p>}
    <ul className={css.presetCards}>
      {presets?.map(preset => <li key={preset.id}>
        <button type="button" aria-pressed={preset.id === selected} className={css.presetCard}
          disabled={!setup?.allowed || !setup.settings_writable || actions.busy || !!preset.broken || preset.id === selected}
          onClick={() => actions.act(() => actions.command({kind:'default_preset', ticket:setup?.ticket, preset:preset.id}))}>
          <strong>{presetName(preset.id, preset)}</strong>
          {preset.description && <span>{preset.description}</span>}
          {preset.broken && <span className={css.presetError}>{preset.broken}</span>}
        </button>
      </li>)}
    </ul>
  </section>;
}
