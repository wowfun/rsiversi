import {useLayoutEffect, useRef, useState} from 'react';
import {input, run, settingsPanel, useView, type Model} from '../bridge.ts';
import {Modal} from '../presentation.tsx';
import type {ComposerOwner, ComposerProjection} from './composer-state.ts';
import {ComposerMenu} from './composer-menu.tsx';
import css from './composer-menu.module.css';
import {IconCheckOutlineRegular as Check, IconChevronDownOutlineRegular as Down, IconChevronLeftOutlineRegular as Back, IconChevronRightOutlineRegular as Next, IconCloseOutlineRegular, IconDataOutlineRegular, SHIELD_OUTLINE_PATH} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';

const sameModel=(a?:Model,b?:Model)=>!!a&&!!b&&a.deployment===b.deployment&&a.model===b.model;
const effortLabel=(value?:string|null)=>value?value[0].toUpperCase()+value.slice(1):'Default';
const Shield=()=> <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><path d={SHIELD_OUTLINE_PATH}/></svg>;
const modes=[['read-only','Read Only'],['workspace-write','Workspace Write'],['danger-full-access','Full access']] as const;

function ModelChoices({current, query, setQuery, busy, disabled, choose, pageModels}: {
  current?: Model;
  query: string;
  setQuery: (value: string) => void;
  busy: boolean;
  disabled: boolean;
  choose: (model: Model, selected: boolean) => void;
  pageModels: (action: 'models_first' | 'models_next') => Promise<void>;
}) {
  const catalog = useView(view => view?.catalog);
  const models = catalog?.models ?? [];
  const choices = current && !models.some(model => sameModel(model, current)) ? [current, ...models] : models;
  const term = query.toLowerCase();
  const filtered = term ? choices.filter(model => `${model.model} ${model.deployment}`.toLowerCase().includes(term)) : choices;
  return <>
    {(choices.length>6||query||catalog?.models_more||!catalog?.models_at_start)&&<input aria-label="Search models on this page" placeholder="Search this page" value={query} onChange={event=>setQuery(event.target.value)}/>}
    {filtered.map(model=>{
      const selected=sameModel(model,current);
      return <button key={JSON.stringify(model)} type="button" role="menuitemradio" aria-checked={selected}
        disabled={disabled} onClick={()=>choose(model,selected)}>
        {model.model}<span className={css.muted}>{model.deployment}</span>{selected&&<Check className={css.trailing}/>}
      </button>;
    })}
    {!filtered.length&&<p>{query?'No matching models on this page.':'No models on this page.'}</p>}
    {catalog?.models_more&&<button type="button" role="menuitem" disabled={busy} onClick={()=>void pageModels('models_next')}>Next page</button>}
    {!catalog?.models_at_start&&<button type="button" role="menuitem" disabled={busy} onClick={()=>void pageModels('models_first')}>First page</button>}
  </>;
}

export function ComposerModel({owner,data}:{owner:ComposerOwner;data?:ComposerProjection|null}) {
  const setup=useView(view=>view?.setup);
  const [page,setPage]=useState<'root'|'models'|'effort'>('root'),[query,setQuery]=useState(''),[busy,setBusy]=useState(false);
  const content=useRef<HTMLDivElement>(null);
  useLayoutEffect(()=>{content.current?.querySelector<HTMLButtonElement>('button')?.focus();},[page]);
  const current=data?.model??setup?.agent?.default_model;
  const startingProfile=sameModel(current,setup?.default_model_description?.model)?setup?.default_model_description?.profile.reasoning_efforts:undefined;
  const profile=data?data.effort_profile:startingProfile;
  const selectedEffort=data?data.reasoning_effort:setup?.agent?.default_reasoning_effort;
  const effort=selectedEffort??profile?.default;
  const pageModels=async(action:'models_first'|'models_next')=>{setBusy(true);await run(()=>input.command({action}));setBusy(false)};
  const disabled=busy||!!owner.switching||!!data?.model_command?.pending||!!owner.composerView?.pending;
  const select=async(model:Model,reasoning_effort:string|null,close:()=>void)=>{
    setBusy(true);
    await run(async()=>{
      if (!await owner.prepareComposer()) return;
      await owner.action('model',{model,reasoning_effort});
      close();
    });
    setBusy(false);
  };
  return <ComposerMenu name={current?`Select model, current ${current.model}${effort?`, reasoning effort ${effortLabel(effort)}`:''}`:'Choose model'} className={css.model}
    label={<><IconDataOutlineRegular size={16} className={css.modelIcon}/><span>{current?.model??'Choose model'}</span>{effort&&<span className={css.muted}>{effortLabel(effort)}</span>}<Down/></>}
    onOpen={async()=>{
      setPage('root');setQuery('');
      if(data){setBusy(true);await run(()=>owner.action('model_refresh'));setBusy(false);}
    }}>
    {(close)=> <div ref={content} onKeyDown={event=>{if(event.defaultPrevented||event.nativeEvent.isComposing||event.target instanceof HTMLElement&&event.target.closest('input,textarea,[contenteditable="true"]'))return;if(event.key==='ArrowLeft'&&page!=='root'){event.preventDefault();setPage('root');}}}>
      {page!=='root'&&<button type="button" role="menuitem" onClick={()=>setPage('root')}><Back/>{page==='models'?'Model':'Reasoning effort'}</button>}
      {page==='root'&&<>
        <button type="button" role="menuitem" onClick={()=>setPage('models')} disabled={disabled}>Model <span className={css.trailing}>{current?.model??'Choose model'}</span><Next/></button>
        {!!profile?.supported.length&&<button type="button" role="menuitem" onClick={()=>setPage('effort')} disabled={disabled}>Effort <span className={css.trailing}>{effortLabel(effort)}</span><Next/></button>}
        <hr/><button type="button" role="menuitem" onClick={()=>{close();settingsPanel.set('models');}}>Manage models</button>
      </>}
      {page==='models'&&<ModelChoices current={current} query={query} setQuery={setQuery}
        busy={busy} disabled={disabled} pageModels={pageModels}
        choose={(model, selected)=>{if(selected)close();else void select(model,null,close)}}/>}
      {page==='effort'&&current&&[null,...profile?.supported??[]].map(value=>{
        const selected=value===(selectedEffort??null);
        const name=value?effortLabel(value):`Default${profile?.default?` (${effortLabel(profile.default)})`:''}`;
        return <button key={value??'default'} type="button" role="menuitemradio" aria-checked={selected}
          disabled={disabled} onClick={selected?close:()=>void select(current,value,close)}>
          {name}{selected&&<Check className={css.trailing}/>}
        </button>;
      })}
      {busy&&<p role="status">Loading…</p>}
    </div>}
  </ComposerMenu>;
}

export function ComposerAccess({owner}:{owner:ComposerOwner}) {
  const [confirm,setConfirm]=useState(false),[acknowledged,setAcknowledged]=useState(false),[busy,setBusy]=useState(false);
  const state=owner.composerView,mode=state?.access;
  const disabled=busy||!mode||state?.accessDisabled;
  const apply=async(value:typeof modes[number][0],close:()=>void)=>{
    setBusy(true);
    await run(async()=>{if(await owner.prepareComposer()){owner.setAccess(value);close();}});
    setBusy(false);
  };
  const label=modes.find(([value])=>value===mode)?.[1]??'Access';
  const dismiss=()=>setConfirm(false);
  return <>
    <ComposerMenu name={`Access mode, current: ${label}`} disabled={state?.accessDisabled} label={<><Shield/><span className={css.accessLabel}>{label}</span><Down/></>}>
      {close=>modes.map(([value,title])=><button key={value} type="button" role="menuitemradio" aria-checked={value===mode} disabled={disabled} onClick={()=>{
        if(value===mode){close();return;}
        if(value==='danger-full-access'){
          close();setAcknowledged(false);setConfirm(true);
        }else void apply(value,close);
      }}>{title}{value===mode&&<Check className={css.trailing}/>}</button>)}
    </ComposerMenu>
    {confirm&&<Modal label="Enable Full access?" className={css.confirm} initialFocus="input" close={dismiss}>
      <header><h2>Enable Full access?</h2><button type="button" aria-label="Close" onClick={dismiss}><IconCloseOutlineRegular/></button></header>
      <p>Tools may operate outside the workspace. Session and tool approval requirements still apply to your next messages.</p>
      <label><input type="checkbox" checked={acknowledged} onChange={event=>setAcknowledged(event.target.checked)}/>I understand the risks and want to continue</label>
      <footer><button type="button" onClick={dismiss}>Cancel</button><button type="button" className="primary" disabled={disabled||!acknowledged} onClick={()=>void apply('danger-full-access',dismiss)}>Enable Full access</button></footer>
    </Modal>}
  </>;
}
