import {useLayoutEffect, useRef, useState} from 'react';
import {run, settingsPanel, useView, type Model} from '../bridge.ts';
import type {ComposerOwner, ComposerProjection} from './composer-state.ts';
import {ComposerMenu} from './composer-menu.tsx';
import css from './composer-menu.module.css';
import {IconCheckOutlineRegular as Check, IconChevronDownOutlineRegular as Down, IconChevronLeftOutlineRegular as Back, IconChevronRightOutlineRegular as Next, IconPlusOutlineRegular as Plus, SHIELD_OUTLINE_PATH} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';

const sameModel=(a?:Model,b?:Model)=>!!a&&!!b&&a.deployment===b.deployment&&a.model===b.model;
const effortLabel=(value?:string|null)=>value?value[0].toUpperCase()+value.slice(1):'Default';
const Shield=()=> <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><path d={SHIELD_OUTLINE_PATH}/></svg>;
const modes=[['read-only','Read Only'],['workspace-write','Workspace Write'],['danger-full-access','Full access']] as const;

export function ComposerModel({owner,data,models}:{owner:ComposerOwner;data?:ComposerProjection|null;models:Model[]}) {
  const setup=useView(view=>view?.setup);
  const [page,setPage]=useState<'root'|'models'|'effort'>('root'),[query,setQuery]=useState(''),[busy,setBusy]=useState(false);
  const content=useRef<HTMLDivElement>(null);
  useLayoutEffect(()=>{content.current?.querySelector<HTMLButtonElement>('button')?.focus();},[page]);
  const current=data?.model??setup?.agent?.default_model;
  const startingProfile=sameModel(current,setup?.default_model_description?.model)?setup?.default_model_description?.profile.reasoning_efforts:undefined;
  const profile=data?data.effort_profile:startingProfile;
  const effort=(data?data.reasoning_effort:setup?.agent?.reasoning_effort)??profile?.default;
  const choices=current&&!models.some(model=>sameModel(model,current))?[current,...models]:models;
  const disabled=busy||!!owner.switching||!!data?.model_command?.pending||!!owner.composerView?.pending;
  const select=async(model:Model,reasoning_effort:string|null,close:()=>void)=>{
    setBusy(true);
    const ok=await run(()=>owner.action('model',{model,reasoning_effort}));
    setBusy(false);if(ok)close();
  };
  return <ComposerMenu name={current?`Select model, current ${current.model}${effort?`, reasoning effort ${effortLabel(effort)}`:''}`:'Choose model'} className={css.model}
    label={<><span>{current?.model??'Choose model'}</span>{effort&&<span className={css.muted}>{effortLabel(effort)}</span>}<Down/></>}
    onOpen={async()=>{
      setPage('root');setQuery('');setBusy(true);
      await run(async()=>{if(await owner.prepareComposer())await owner.action('model_refresh');});setBusy(false);
    }}>
    {(close)=> <div ref={content} onKeyDown={event=>{if(event.key==='ArrowLeft'&&page!=='root'){event.preventDefault();setPage('root');}}}>
      {page!=='root'&&<button type="button" role="menuitem" onClick={()=>setPage('root')}><Back/>{page==='models'?'Model':'Reasoning effort'}</button>}
      {page==='root'&&<>
        <button type="button" role="menuitem" onClick={()=>setPage('models')} disabled={disabled}>Model <span className={css.trailing}>{current?.model??'Choose model'}</span><Next/></button>
        {!!data?.effort_profile?.supported.length&&<button type="button" role="menuitem" onClick={()=>setPage('effort')} disabled={disabled}>Effort <span className={css.trailing}>{effortLabel(effort)}</span><Next/></button>}
        <hr/><button type="button" role="menuitem" onClick={()=>{close();settingsPanel.set('models');}}>Manage models</button>
      </>}
      {page==='models'&&<>
        {choices.length>6&&<input aria-label="Search models" value={query} onChange={event=>setQuery(event.target.value)}/>}
        {choices.filter(model=>`${model.model} ${model.deployment}`.toLowerCase().includes(query.toLowerCase())).map(model=><button key={JSON.stringify(model)} type="button" role="menuitemradio" aria-checked={sameModel(model,current)} disabled={disabled||!data} onClick={()=>void select(model,null,close)}>{model.model}<span className={css.muted}>{model.deployment}</span>{sameModel(model,current)&&<Check className={css.trailing}/>}</button>)}
        {!choices.length&&<p>No models configured.</p>}
      </>}
      {page==='effort'&&data&&[null,...data.effort_profile?.supported??[]].map(value=><button key={value??'default'} type="button" role="menuitemradio" aria-checked={value===(data.reasoning_effort??null)} disabled={disabled} onClick={()=>void select(data.model,value,close)}>{value?effortLabel(value):`Default${data.effort_profile?.default?` (${effortLabel(data.effort_profile.default)})`:''}`}{value===(data.reasoning_effort??null)&&<Check className={css.trailing}/>}</button>)}
      {busy&&<p role="status">Loading…</p>}
    </div>}
  </ComposerMenu>;
}

export function ComposerAccess({owner}:{owner:ComposerOwner}) {
  const [confirm,setConfirm]=useState(false),[ready,setReady]=useState(false);
  const state=owner.composerView,mode=state?.access;
  const disabled=!ready||!mode||state?.accessDisabled;
  const label=modes.find(([value])=>value===mode)?.[1]??'Access';
  return <ComposerMenu name={`Access mode, current: ${label}`} disabled={state?.accessDisabled} label={<><Shield/>{label}<Down/></>}
    onOpen={async()=>{setConfirm(false);setReady(false);await run(async()=>{setReady(await owner.prepareComposer());});}}>
    {close=>confirm?<div className={css.confirm}>
      <strong>Allow full access?</strong><p>Tools may operate outside the workspace. RSI still asks for tool approval. This applies to your next messages.</p>
      <button type="button" role="menuitem" onClick={()=>setConfirm(false)}>Cancel</button>
      <button type="button" role="menuitem" className={css.danger} disabled={disabled} onClick={()=>{if(mode)void run(()=>{owner.setAccess('danger-full-access');close();});}}>Allow full access</button>
    </div>:<>{modes.map(([value,title])=><button key={value} type="button" role="menuitemradio" aria-checked={value===mode} disabled={disabled} onClick={()=>{if(value==='danger-full-access'&&value!==mode)setConfirm(true);else void run(()=>{owner.setAccess(value);close();});}}>{title}{value===mode&&<Check className={css.trailing}/>}</button>)}<hr/><p>Applies to new turns; steering keeps the active turn’s access.</p></>}
  </ComposerMenu>;
}

export function ComposerAdd({owner,data,register}:{owner:ComposerOwner;data?:ComposerProjection|null;register:(name:string)=>(node:HTMLElement|null)=>void}) {
  const [commands,setCommands]=useState(false),state=owner.composerView;
  const back=useRef<HTMLButtonElement>(null);
  useLayoutEffect(()=>{if(commands)back.current?.focus();},[commands]);
  return <ComposerMenu name="Add files or run commands" className={css.plus} label={<Plus/>} onOpen={async()=>{setCommands(false);await run(async()=>{if(await owner.prepareComposer())await owner.action('commands');});}}>
    {close=><>
      <div hidden={commands}>
        <div className={css.heading}>Add</div>
        {([['attach','Images',state?.imageDisabled,!state?.mediaEnabled],['addFile','File',state?.fileDisabled,false],['addReference','Conversation',state?.referenceDisabled,false],['searchHistory','Search history',state?.referenceDisabled,false],['exportButton',state?.exporting?'Cancel export':'Export',!data,false]] as const).map(([name,label,disabled,hidden])=><button type="button" role="menuitem" key={name} ref={register(name)} disabled={disabled??true} hidden={hidden} onClick={close}>{label}</button>)}
        <hr/><button type="button" role="menuitem" disabled={!data?.commands} onClick={()=>setCommands(true)}>Commands<Next className={css.trailing}/></button>
      </div>
      <div hidden={!commands} onKeyDown={event=>{if(event.key==='ArrowLeft'){event.preventDefault();setCommands(false);}}}>
        <button ref={back} type="button" role="menuitem" onClick={()=>setCommands(false)}><Back/>Commands</button>
        {data?.commands?.commands.map(command=><button type="button" role="menuitem" key={command.name} disabled={data.commands?.revision.kind==='draft'&&!command.draft_safe} title={command.description} onClick={()=>{close();owner.selectCommand(command.name);}}>/{command.name}</button>)}
      </div>
    </>}
  </ComposerMenu>;
}
