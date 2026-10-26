import {useEffect,useRef,useState} from 'react'
import {Terminal as Xterm} from '@xterm/xterm'
import {FitAddon} from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import {Button} from './button.tsx'
import {type Surface} from './bridge.ts'
import {readTerminalPage} from './terminal-read.mjs'
import {TerminalInput} from './terminal-input.mjs'
import {terminalDocument} from './terminal-style.ts'
import {ResourceOwner} from './resource-owner.mjs'

type Status={id:string;size:{rows:number;columns:number};phase:{state:string;exit_code?:number|null;signal?:number|null};controller:string|null;controller_epoch:number}
export type Follower={terminal:Status;id:string}
type Reply={type:string;value:any;ack?:string}
function terminalTheme(){const style=getComputedStyle(document.documentElement);return {background:style.getPropertyValue('--secondary-surface').trim(),foreground:style.getPropertyValue('--ink').trim(),cursor:style.getPropertyValue('--teal').trim(),selectionBackground:style.getPropertyValue('--active').trim()}}
const encoder=new TextEncoder()
export function Terminals({owner,pane,surface,hide,initial,created,onOpen}:{owner:ResourceOwner;pane:string;surface:Surface;hide:()=>void;initial?:string;created?:ReturnType<ResourceOwner['attachment']>;onOpen?:(id?:string)=>void}) {
  const [roster,setRoster]=useState<Status[]>([]),[follower,setFollower]=useState<Follower>(),[status,setStatus]=useState<Status>(),[notice,setNotice]=useState(''),[busy,setBusy]=useState(false)
  const screen=useRef<HTMLDivElement>(null),alive=useRef(true),current=useRef<Follower>(),pump=useRef<TerminalInput>(),display=useRef<Xterm>(),resizeCurrent=useRef<()=>void>(),attachment=useRef<ReturnType<ResourceOwner['attachment']>>()
  const request=async(request:unknown):Promise<Reply>=>JSON.parse(await owner.call('terminal',JSON.stringify({pane,generation:surface.generation,request})))
  const operate=(operation:unknown)=>request(operation)
  const refresh=async()=>{const reply=await operate({type:'list'});if(alive.current)setRoster(reply.value)}
  const detach=()=>{attachment.current?.close();attachment.current=undefined}
  useEffect(()=>{alive.current=true;if(created){attachment.current=created;void created.promise.then((value:Follower)=>{if(!alive.current){created.close();return}current.current=value;setFollower(value);setStatus(value.terminal)}).catch(error=>{if(alive.current)setNotice(String(error))})}else if(initial){void attach(initial).catch(error=>{if(alive.current)setNotice(String(error))})}void refresh().catch(error=>setNotice(String(error)));return()=>{alive.current=false;pump.current?.stop();detach()}},[])
  const action=async(work:()=>Promise<void>)=>{if(busy)return;setBusy(true);setNotice('');try{await work()}catch(error){if(alive.current)setNotice(String(error))}finally{if(alive.current)setBusy(false)}}
  const attach=async(id?:string)=>{
    pump.current?.stop();if(current.current){detach();current.current=undefined;setFollower(undefined)}
    const lease=owner.attachment(surface.session,pane,surface.generation,id?{type:'attach',terminal:id}:{type:'create',size:{rows:24,columns:80}});attachment.current=lease
    const value=await lease.promise as Follower
    if(!alive.current){lease.close();return}
    current.current=value;setFollower(value);setStatus(value.terminal);await refresh()
  }
  const installWriter=(value:Follower,terminal:Status)=>{
    pump.current?.stop()
    if(terminal.controller!==value.id||terminal.phase.state!=='running'){if(display.current)display.current.options.disableStdin=true;return}
    const coordinate={attachment:value.id}
    pump.current=new TerminalInput(
      async(bytes:Uint8Array)=>{await operate({type:'write',...coordinate,bytes:Array.from(bytes)})},
      (message:string)=>{if(alive.current)setNotice(message);if(display.current)display.current.options.disableStdin=true},
    )
    if(display.current)display.current.options.disableStdin=false
  }
  useEffect(()=>{
    if(!follower||!screen.current)return
    let active=true,ack:string|undefined,resizeTimer:ReturnType<typeof setTimeout>|undefined
    const styles=terminalDocument(document)
    const term=new Xterm({documentOverride:styles.document,fontFamily:'"SFMono-Regular",Consolas,"Liberation Mono",monospace',fontSize:12,lineHeight:1.2,scrollback:1000,rows:follower.terminal.size.rows,cols:follower.terminal.size.columns,disableStdin:true,convertEol:false,theme:terminalTheme()})
    const fit=new FitAddon()
    try{term.loadAddon(fit);term.open(screen.current)}catch(error){term.dispose();styles.dispose();setNotice(String(error));return}
    display.current=term;installWriter(follower,follower.terminal)
    const data=term.onData(text=>pump.current?.push(encoder.encode(text))),binary=term.onBinary(text=>pump.current?.push(Uint8Array.from(text,c=>c.charCodeAt(0)&255)))
    const resize=()=>{clearTimeout(resizeTimer);resizeTimer=setTimeout(()=>{
      const value=current.current,bounds=screen.current?.getBoundingClientRect()
      if(!active||!value||!bounds||bounds.width<=0||bounds.height<=0||value.terminal.controller!==value.id||value.terminal.phase.state!=='running')return
      const dimensions=fit.proposeDimensions()
      // FitAddon parses computed CSS sizes; hidden/transitioning hosts can yield
      // NaN even after xterm has cached valid cell metrics.
      if(!dimensions||!Number.isFinite(dimensions.rows)||!Number.isFinite(dimensions.cols))return
      const size={rows:Math.max(1,Math.min(200,Math.floor(dimensions.rows))),columns:Math.max(1,Math.min(500,Math.floor(dimensions.cols)))}
      if(size.rows===value.terminal.size.rows&&size.columns===value.terminal.size.columns)return
      void operate({type:'resize',attachment:value.id,size}).catch(error=>{if(active)setNotice(String(error))})
    },100)}
    resizeCurrent.current=resize
    const observer=new ResizeObserver(resize);observer.observe(screen.current);resize()
    const refreshTheme=()=>{term.options.theme=terminalTheme()}
    const themeObserver=new MutationObserver(refreshTheme);themeObserver.observe(document.documentElement,{attributes:true,attributeFilter:['data-theme']})
    const systemTheme=matchMedia('(prefers-color-scheme: dark)');systemTheme.addEventListener('change',refreshTheme)
    void(async()=>{while(active){
      try{
        const page=await readTerminalPage((ticket:string|undefined)=>operate({type:'read',attachment:follower.id,ack:ticket??null}),ack,()=>active)
        if(!active||!page)return
        const {value,ack:drawn}=page
        const previous=current.current?.terminal;if(previous && value.terminal.controller_epoch<previous.controller_epoch)value.terminal=previous;follower.terminal=value.terminal;current.current=follower;setStatus(value.terminal)
        if(previous?.controller_epoch!==value.terminal.controller_epoch||value.terminal.controller!==follower.id||value.terminal.phase.state!=='running'){pump.current?.stop();term.options.disableStdin=true}
        term.resize(value.terminal.size.columns,value.terminal.size.rows)
        if(value.reset)term.reset()
        // Await xterm's parser so output cannot outrun document rendering.
        if(value.text)await new Promise<void>(resolve=>term.write(value.text,resolve))
        ack=drawn
        if(value.terminal.phase.state!=='running'&&!value.text){await refresh();return}
      }catch(error){if(active){setNotice(String(error));pump.current?.stop();term.options.disableStdin=true}return}
    }})()
    return()=>{active=false;themeObserver.disconnect();systemTheme.removeEventListener('change',refreshTheme);clearTimeout(resizeTimer);observer.disconnect();data.dispose();binary.dispose();pump.current?.stop();term.dispose();styles.dispose();display.current=undefined;resizeCurrent.current=undefined}
  },[follower?.id])
  const writer=!!follower&&status?.controller===follower.id&&status.phase.state==='running'
  const phase=status?.phase.state==='exited'?`Exited ${status.phase.exit_code??`(signal ${status.phase.signal??'unknown'})`}`:status?.phase.state==='failed'?'Stopped with an error':writer?'You have control':status?'Read only':'No terminal selected'
  return <section className="terminal-panel" aria-label="Session terminals">
    <div className="terminal-toolbar"><strong>Terminal</strong><span className={`terminal-authority ${writer?'is-writer':''}`} role="status">{phase}</span><div className="terminal-actions"><Button size="sm" disabled={busy} onClick={()=>onOpen?onOpen():void action(()=>attach())}>New terminal</Button><Button size="sm" disabled={busy} onClick={()=>void action(refresh)}>Refresh</Button>{follower&&status?.phase.state==='running'&&<Button size="sm" disabled={busy} onClick={()=>void action(async()=>{const reply=await operate({type:'takeover',attachment:follower.id});follower.terminal=reply.value;current.current=follower;setStatus(reply.value);installWriter(follower,reply.value);resizeCurrent.current?.();display.current?.focus()})}>Take control</Button>}{follower&&<Button size="sm" disabled={busy} onClick={()=>void action(async()=>{await operate({type:'close',terminal:follower.terminal.id});pump.current?.stop();detach();current.current=undefined;setFollower(undefined);setStatus(undefined);await refresh()})}>Terminate terminal</Button>}<Button size="sm" onClick={hide} aria-label="Hide terminal panel">×</Button></div></div>
    {!initial&&roster.length>0&&<nav className="terminal-tabs" aria-label="Open terminals">{roster.map((terminal,index)=><Button size="sm" key={terminal.id} aria-pressed={follower?.terminal.id===terminal.id} title={terminal.id} disabled={busy} onClick={()=>onOpen?onOpen(terminal.id):void action(()=>attach(terminal.id))}>Bash {index+1} · {terminal.phase.state}</Button>)}</nav>}
    {notice&&<p className="terminal-notice" role="alert">{notice}</p>}
    <div className="terminal-screen" ref={screen} hidden={!follower}/>
    {!follower&&<div className="terminal-empty"><p>Run commands in this Session’s workspace.</p><p className="hint">New terminals use this Session’s saved sandbox policy. Hiding this panel leaves shells running.</p></div>}
    <footer className="terminal-footer"><span title={surface.path}>{surface.path}</span><span>{status?`${status.size.columns} × ${status.size.rows}`:'Bash · ANSI/VT100'}</span></footer>
  </section>
}
