import {useEffect, useId, useLayoutEffect, useRef, useState, type HTMLAttributes} from 'react';
import {createPortal} from 'react-dom';
import {input, run, type WorkspaceRecord} from './bridge.ts';
import {Modal} from './presentation.tsx';
import {Button} from './button.tsx';
import {IconEllipsisOutlineRegular, IconEditOutlineRegular, IconTrashOutlineRegular, IconCloseOutlineRegular, IconNewChatOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './workspace-header.module.css';

export function WorkspaceHeader({record, name, children, ...props}: HTMLAttributes<HTMLDivElement> & {record?:WorkspaceRecord;name:string}) {
  const root=useRef<HTMLDivElement>(null), trigger=useRef<HTMLButtonElement>(null), floating=useRef<HTMLDivElement>(null);
  const timer=useRef<ReturnType<typeof setTimeout>>(), id=useId();
  const [overlay,setOverlay]=useState<'info'|'menu'>(), [edit,setEdit]=useState<'rename'|'delete'>();
  const clear=()=>{clearTimeout(timer.current);timer.current=undefined;};
  const hide=()=>{clear();setOverlay(undefined);};
  const show=()=>{clear();if(record&&!edit&&overlay!=='menu')timer.current=setTimeout(()=>setOverlay('info'),800);};
  useEffect(()=>()=>clear(),[]);
  useLayoutEffect(()=>{
    if(!overlay)return;
    const place=()=>{
      const anchor=(overlay==='menu'?trigger:root).current?.getBoundingClientRect(), panel=floating.current;
      if(!anchor||!panel)return;
      const box=panel.getBoundingClientRect(), margin=8;
      const left=overlay==='info'?anchor.right+8:anchor.right-box.width;
      panel.style.left=`${Math.max(margin,Math.min(left,innerWidth-box.width-margin))}px`;
      const top=overlay==='info'?anchor.top:anchor.bottom+4;
      panel.style.top=`${Math.max(margin,Math.min(top,innerHeight-box.height-margin))}px`;
      panel.style.visibility='visible';
    };
    const outside=(event:Event)=>{if(event.target instanceof Node&&!root.current?.contains(event.target)&&!floating.current?.contains(event.target))hide();};
    const key=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();hide();if(overlay==='menu')trigger.current?.focus();}};
    place();
    if(overlay==='menu')floating.current?.querySelector<HTMLButtonElement>('button')?.focus();
    window.addEventListener('resize',place);window.addEventListener('scroll',place,true);
    document.addEventListener('pointerdown',outside);document.addEventListener('focusin',outside);document.addEventListener('keydown',key);
    return()=>{window.removeEventListener('resize',place);window.removeEventListener('scroll',place,true);document.removeEventListener('pointerdown',outside);document.removeEventListener('focusin',outside);document.removeEventListener('keydown',key);};
  },[overlay]);
  const choose=(kind:'rename'|'delete')=>{hide();trigger.current?.focus();setEdit(kind);};
  const created=record?.created_at_ms == null?null:new Date(record.created_at_ms);
  return <>
    <div {...props} ref={root} data-menu-open={overlay==='menu'||undefined}
      onPointerEnter={show} onPointerLeave={()=>{clear();if(overlay==='info')timer.current=setTimeout(hide,160);}}
      onPointerDown={()=>{clear();if(overlay==='info')hide();}}
      onFocus={event=>{if(record&&event.target instanceof HTMLElement&&event.target.matches('.workspace-toggle')){clear();setOverlay('info');}}}
      aria-describedby={overlay==='info'?id:undefined}>
      {children}
      {record&&<><button ref={trigger} type="button" className={css.action} aria-label={`Workspace actions for ${name}`} aria-haspopup="menu" aria-expanded={overlay==='menu'} onClick={()=>{clear();setOverlay(overlay==='menu'?undefined:'menu');}}><IconEllipsisOutlineRegular/></button>
      <Button size="sm" aria-label={`New conversation in ${name}`} data-testid="workspace-open" onClick={()=>run(()=>input.open({action:'create',workspace:record.id}))}><IconNewChatOutlineRegular/></Button></>}
    </div>
    {overlay&&record&&createPortal(<div ref={floating} id={id} className={overlay==='info'?css.info:css.menu} role={overlay==='info'?'tooltip':'menu'} aria-label={overlay==='menu'?`Workspace actions for ${name}`:undefined}
      onPointerEnter={clear} onPointerLeave={()=>{if(overlay==='info')hide();}}
      onKeyDown={event=>{if(overlay!=='menu'||!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;event.preventDefault();const buttons=Array.from(floating.current!.querySelectorAll('button'));const current=buttons.indexOf(document.activeElement as HTMLButtonElement);buttons[event.key==='Home'?0:event.key==='End'?buttons.length-1:(current+(event.key==='ArrowDown'?1:-1)+buttons.length)%buttons.length]?.focus();}}>
      {overlay==='info'?<><strong>{name}</strong><span>{record.coordinates.path}</span>{record.coordinates.location.kind==='ssh'&&<span>SSH · {record.coordinates.location.target}</span>}{created&&<span>Created {created.getFullYear()}-{String(created.getMonth()+1).padStart(2,'0')}-{String(created.getDate()).padStart(2,'0')} {String(created.getHours()).padStart(2,'0')}:{String(created.getMinutes()).padStart(2,'0')}</span>}</>:
      <><button type="button" role="menuitem" onClick={()=>choose('rename')}><IconEditOutlineRegular/>Rename</button><button type="button" role="menuitem" className={css.danger} onClick={()=>choose('delete')}><IconTrashOutlineRegular/>Delete workspace</button></>}
    </div>,document.body)}
    {edit&&record&&<WorkspaceDialog kind={edit} record={record} name={name} close={()=>setEdit(undefined)}/>}
  </>;
}

function WorkspaceDialog({kind,record,name,close}:{kind:'rename'|'delete';record:WorkspaceRecord;name:string;close:()=>void}) {
  const [value,setValue]=useState(name),[busy,setBusy]=useState(false),[error,setError]=useState('');
  const pending=useRef(false), alive=useRef(true);
  useEffect(()=>()=>{alive.current=false;},[]);
  const label=kind==='rename'?'Rename workspace':'Delete workspace';
  const submit=async()=>{
    if(pending.current)return;
    const trimmed=value.trim();
    if(kind==='rename'&&(!trimmed||new TextEncoder().encode(trimmed).length>256||/[\x00-\x1f\x7f-\x9f]/.test(trimmed))){setError('Use a name of at most 256 bytes without control characters.');return;}
    pending.current=true;setBusy(true);setError('');
    try{await input.command(kind==='rename'?{action:'rename_workspace',workspace:record.id,name:trimmed}:{action:'delete_workspace',workspace:record.id});if(alive.current)close();}
    catch(reason){if(alive.current)setError(reason instanceof Error?reason.message:String(reason));}
    finally{pending.current=false;if(alive.current)setBusy(false);}
  };
  const dismiss=()=>{if(!pending.current)close();};
  return <Modal label={label} className={css.dialog} close={dismiss} initialFocus={kind==='rename'?'input':'[data-cancel]'}>
    <form onSubmit={event=>{event.preventDefault();void submit();}} aria-busy={busy}>
      <header><h2>{label}</h2><button type="button" aria-label={`Close ${label.toLowerCase()}`} disabled={busy} onClick={dismiss}><IconCloseOutlineRegular/></button></header>
      {kind==='rename'?<input aria-label="Workspace name" value={value} disabled={busy} maxLength={256} onFocus={event=>event.target.select()} onChange={event=>setValue(event.target.value)} required/>:<p>This removes “{name}” from the workspace list. The folder and session logs will be kept. Its sessions will appear under Ungrouped.</p>}
      {error&&<p role="alert" className={css.danger}>{error}</p>}
      <footer><Button variant="outline" data-cancel disabled={busy} onClick={dismiss}>Cancel</Button><Button type="submit" variant={kind==='rename'?'primary':'outline'} className={kind==='delete'?css.danger:undefined} disabled={busy||kind==='rename'&&!value.trim()}>{busy?'Saving…':kind==='rename'?'Save':'Delete workspace'}</Button></footer>
    </form>
  </Modal>;
}
