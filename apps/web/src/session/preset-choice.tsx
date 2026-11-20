import { useEffect, useState } from 'react';
import { input, run, useView, type AgentPreset } from '../bridge.ts';
import { ComposerMenu } from './composer-menu.tsx';
import { IconAgentPresetOutlineRegular, IconCheckOutlineRegular, IconChevronDownOutlineRegular } from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import type { ComposerOwner } from './composer-state.ts';

export function presetName(id: string, preset?: AgentPreset) {
  return id === 'standard' && (!preset?.name || preset.name === 'Standard') ? 'Standard mode' : preset?.name ?? id;
}

function usePreset(id: string | undefined, presets: AgentPreset[] | undefined) {
  useEffect(() => {
    if (!presets && id && id !== 'standard') void input.command({action:'presets'}).catch(() => {});
  }, [id, presets]);
  return presets?.find(preset => preset.id === id);
}

export function PresetLabel({id}: {id: string}) {
  const presets = useView(view => view?.catalog.presets?.presets);
  const preset = usePreset(id,presets);
  return <span className="session-preset" title={preset?.description ?? id}>
    <IconAgentPresetOutlineRegular size={14}/>{presetName(id, preset)}
  </span>;
}

export function PresetChoice({owner, applied}: {owner: ComposerOwner; applied?: string}) {
  const presets = useView(view => view?.catalog.presets?.presets);
  const configured = useView(view => view?.setup?.presets?.default);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const id = applied ?? owner.stagedPreset ?? configured ?? presets?.find(preset => preset.is_default)?.id;
  const current = usePreset(id,presets);
  const label = id ? presetName(id, current) : 'Default preset';
  const read = async () => {
    setLoading(true);
    setError('');
    try { await input.command({action:'presets'}); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setLoading(false); }
  };
  return <ComposerMenu name={label} className="preset-choice" focusSelection onOpen={read}
    disabled={!!owner.switching || !!owner.composerView?.disabled}
    label={<><IconAgentPresetOutlineRegular/>{label}<IconChevronDownOutlineRegular/></>}>
    {close => loading ? <p role="status">Loading presets…</p> : error ? <p role="status">{error}</p> : <>
      {presets?.filter(preset => !preset.broken).map(preset =>
        <button type="button" role="menuitemradio" aria-checked={preset.id === id} key={preset.id}
          onClick={() => { close(); void run(() => owner.selectPreset(preset.id)); }}>
          <span className="preset-description"><strong>{presetName(preset.id, preset)}</strong>{preset.description && <small>{preset.description}</small>}</span>
          {preset.id === id && <IconCheckOutlineRegular/>}
        </button>)}
      {presets && !presets.some(preset => !preset.broken) && <p>No presets available</p>}
    </>}
  </ComposerMenu>;
}
