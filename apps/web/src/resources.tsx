import {useEffect, useId, useState, type ReactNode} from 'react';
import {input, run, useView} from './bridge.ts';
import type {ExternalSurface, NativeSurface} from './presentation-types.ts';
import {Menu, MenuItemButton} from '../vendor/dsh/dockkit-primitives/Menu.tsx';
import {CompassGlyph} from '../vendor/dsh/guide/Compass.tsx';
import {GuideMoreIcon} from '../vendor/dsh/guide/GuideMoreIcon.tsx';
import {GuideArtworkFiles} from '../vendor/dsh/guide/Files.tsx';
import {PluginArtworkTerminal} from '../vendor/dsh/guide/Terminal.tsx';
import css from '../vendor/dsh/guide/GuideBody.module.css';
import styles from './resources.module.css';

export function ExternalResources({surface}: {surface:ExternalSurface}) {
  const external=surface.external, snapshot=external.observed.snapshot;
  return <div className="external-resources">
    <h2>External conversation</h2><p className="resource-path">{snapshot.cwd}</p>
    <dl className="session-facts">
      <dt>Endpoint</dt><dd>{snapshot.endpoint}</dd>
      <dt>Connection</dt><dd>{external.observed.connected?'Connected':'Disconnected'}</dd>
      <dt>Observation</dt><dd>{snapshot.status}</dd>
      <dt>Visible records</dt><dd>{external.blocks.length}</dd>
    </dl>
    <p className="hint">Switching views keeps the agent connected. Use Close peer to end its connection.</p>
  </div>;
}

function GuideEntry({title,description,icon,onClick}: {title:string;description:string;icon:ReactNode;onClick:()=>void}) {
  const id=useId();
  return <button type="button" className={css.entry} aria-label={title} aria-describedby={id} onClick={onClick}>
    <span className={css.entryIcon} aria-hidden="true">{icon}</span>
    <span className={css.entryText}><span className={css.entryTitle}>{title}</span>
      <span id={id} className={css.entryDescription}>{description}</span></span>
  </button>;
}

type ShellRow={id:string;phase:{state:string}};
export interface GuideTerminals {
  open(id?:string):void;
  list():Promise<{value:ShellRow[]}>;
}
type Roster={phase:'idle'|'loading'}|{phase:'ready';rows:ShellRow[]}|{phase:'failed';message:string};
function RetainedTerminals({list,onOpen}:{list:GuideTerminals['list'];onOpen:(id:string)=>void}) {
  const [roster,setRoster]=useState<Roster>({phase:'idle'});
  const loading=roster.phase==='loading';
  useEffect(()=>{
    if(!loading)return;
    let active=true;
    void list().then(result=>{if(active)setRoster({phase:'ready',rows:result.value})},
      error=>{if(active)setRoster({phase:'failed',message:String(error)})});
    return ()=>{active=false};
  },[list,loading]);
  return <>
    <MenuItemButton separatorBefore disabled={loading} disabledReason="Loading terminals" onSelect={()=>setRoster({phase:'loading'})}>
      {roster.phase==='idle'?'List terminals':loading?'Loading terminals…':roster.phase==='failed'?'Retry loading terminals':'Refresh terminals'}
    </MenuItemButton>
    {roster.phase==='ready'&&(roster.rows.length?roster.rows.map((shell,index)=><MenuItemButton key={shell.id} onSelect={()=>onOpen(shell.id)}>Terminal {index+1} · {shell.phase.state}</MenuItemButton>):
      <p role="status" className={styles.rosterStatus}>No retained terminals</p>)}
    {roster.phase==='failed'&&<p role="status" className={styles.rosterStatus}>{roster.message}</p>}
  </>;
}
export function ResourceGuide({surface,pane,terminals}: {surface:NativeSurface;pane:string;terminals?:GuideTerminals}) {
  const remote=useView(view=>view?.has_remote_ui);
  const [open,setOpen]=useState(false);
  const files=surface.ui_surfaces.find(item=>item.bundle==='rsi.session.files' && item.reference.name==='files');
  const invoke=(action:string,fields={})=>void run(()=>input.command({action,pane,generation:surface.generation,...fields}));
  const inspect=(item:NativeSurface['ui_surfaces'][number])=>invoke('ui_surface',{reference:item.reference});
  const select=(action:()=>void)=>()=>{setOpen(false);action()};
  return <div className={css.guide+' resource-guide'} aria-label="Session resource guide">
    <span className={css.hero} aria-hidden="true"><CompassGlyph size={56}/></span>
    {files&&<GuideEntry title={files.title} description="Browse files in this session’s workspace" icon={<GuideArtworkFiles size={26}/>} onClick={()=>inspect(files)}/>}
    {terminals&&<GuideEntry title="New terminal" description="Run commands in the Session workspace" icon={<PluginArtworkTerminal size={26}/>} onClick={()=>terminals.open()}/>}
    <Menu open={open} onClose={()=>setOpen(false)} portal autoFocus minWidthFromAnchor align="end"
      className={css.moreAnchor} listClassName={css.moreMenu} anchor={
        <button type="button" className={css.entry} aria-haspopup="menu" aria-expanded={open} onClick={()=>setOpen(value=>!value)}>
          <span className={css.entryIcon}><GuideMoreIcon/></span><span className={css.entryTitle}>More tools</span>
        </button>}>
      {open&&<>
        {surface.ui_surfaces.map(item=>item!==files&&<MenuItemButton key={JSON.stringify(item.reference)} onSelect={select(()=>inspect(item))}>{item.title}</MenuItemButton>)}
        <MenuItemButton onSelect={select(()=>invoke('commands'))}>Session commands</MenuItemButton>
        {remote&&<MenuItemButton onSelect={select(()=>invoke('remote_ui_list'))}>Service extensions</MenuItemButton>}
        {terminals&&<RetainedTerminals list={terminals.list} onOpen={id=>{setOpen(false);terminals.open(id)}}/>}
      </>}
    </Menu>
  </div>;
}
