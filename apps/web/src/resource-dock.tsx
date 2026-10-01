import {useEffect,useLayoutEffect,useMemo,useRef,useState,useSyncExternalStore,type ReactNode} from 'react'
import {DockLayout} from '../vendor/dsh/dockkit/components/DockSurface.tsx'
import type {DockIntents,DockLabels} from '../vendor/dsh/dockkit/contract/adapter.ts'
import type {TabId,TabRecord} from '../vendor/dsh/dockkit/contract/types.ts'
import {DeviceStore} from '../device-store.js'
import {coordinate,dockIntent,emptyDock,floatingTab,dockPaneIds,displayDock,paneFor,getPane,type DockDocument,type DockIntent,type Coordinate} from './dock-state.ts'
import {resources,resourceHosts,type ResourceHost} from './resource-hosts.ts'
import {input,source,useSelected,useView,type Surface} from './bridge.ts'
import {presentationIdentity,useLayout,useNarrow,useViewport} from './presentation.tsx'
import {Terminals,type Follower} from './terminal.tsx'
import {Button} from './button.tsx'

const labels:DockLabels={emptyPane:'Open a resource to use this pane',splitPane:'Split resources',splitPaneDisabled:'Two resource panes are open',splitPaneNarrow:'Widen resources to split',closeTab:'Close resource tab',addTab:'Open resources',dockFloat:'Dock resource',closeFloat:'Close resource window',dropZone:{center:'Merge tabs',left:'Split left',right:'Split right',top:'',bottom:''}}
const descriptor=(host:ResourceHost,surface:Surface):Coordinate=>{
  const value=host.entry.reopen
  if(!value)throw new Error('This resource cannot be reopened')
  if(value.action==='ui_surface'||value.action==='application_ui_surface'){
    const candidates=value.action==='ui_surface'?surface.ui_surfaces:source.getSnapshot()?.application_surfaces??[]
    const entry=candidates.find(item=>JSON.stringify(item.reference)===JSON.stringify(value.reference))
    if(!entry)throw new Error('This extension has retired')
    return {action:value.action==='ui_surface'?'surface':'application_surface',bundle:entry.bundle,name:entry.reference.name}
  }
  const {pane:_,generation:__,...saved}=value
  return coordinate(saved)
}
function ResourceBody({view}:{view:string}) {
  const root=useRef<HTMLDivElement>(null)
  useLayoutEffect(()=>{const host=resourceHosts.get(view);if(host&&root.current)root.current.append(host.body);return()=>{if(host?.body.isConnected)document.getElementById('resource-parking')?.append(host.body)}},[view])
  return <div className="resource-host" ref={root}/>
}
interface Binding {view?:string;created?:Follower;error?:string;requested?:boolean}
export function openTerminals() { window.dispatchEvent(new Event('rsi:terminals')) }
export function ResourceDocks({launcher}:{launcher:ReactNode}) {
  const surfaces=useView(view=>view?.surfaces),selected=useSelected(value=>value)
  return <>{Object.entries(surfaces??{}).map(([pane,surface])=>surface&&surface.kind!=='external'&&<SessionDock key={`${pane}:${surface.generation}`} pane={pane} surface={surface} active={pane===selected} launcher={launcher}/>)}</>
}
function SessionDock({pane,surface,active,launcher}:{pane:string;surface:Surface;active:boolean;launcher:ReactNode}) {
  const identity=useSyncExternalStore(presentationIdentity.subscribe,presentationIdentity.getSnapshot)
  const hosts=useSyncExternalStore(resources.subscribe,resources.getSnapshot)
  const [doc,setDoc]=useState<DockDocument>(emptyDock),[notice,setNotice]=useState(''),[revision,changed]=useState(0)
  const current=useRef(doc),store=useRef<DeviceStore>(),queue=useRef(Promise.resolve()),alive=useRef(true),ready=useRef(false),generation=useRef(0)
  const bindings=useRef(new Map<TabId,Binding>()),claimed=useRef(new Set<string>()),hostList=useRef(hosts),activeRef=useRef(active)
  const {layout,update}=useLayout(),narrow=useNarrow();activeRef.current=active;hostList.current=hosts
  const viewport=useViewport()
  const scope=`dock:${surface.session}`
  const relevant=(host:ResourceHost)=>host.entry.session===surface.session&&host.entry.pane===pane&&host.entry.generation===surface.generation||host.entry.session===null&&pane==='main'
  const command=(value:Record<string,unknown>)=>input.command({...value,pane,generation:surface.generation})
  const terminal=async(request:unknown)=>JSON.parse(await input.terminal({pane,generation:surface.generation,request}))
  const publish=(next:DockDocument)=>{current.current=next;if(alive.current)setDoc(next)}
  const enqueue=(work:(valid:()=>boolean)=>Promise<void>)=>{const epoch=generation.current,valid=()=>alive.current&&generation.current===epoch;queue.current=queue.current.then(async()=>{if(valid())await work(valid)}).catch(error=>{if(valid())setNotice(String(error))})}
  const save=async(intent:DockIntent)=>{
    const owner=store.current
    // A failed save is reported and the previously committed layout is retained.
    if(owner)return (await owner.apply('layouts',intent,scope)).value as DockDocument
    setNotice('Resource layout is not saved on this device.')
    return dockIntent(current.current,intent)
  }
  const reopen=async(c:Coordinate)=>{
    if(c.action==='surface'||c.action==='application_surface'){
      const candidates=c.action==='surface'?source.getSnapshot()?.surfaces[pane]?.ui_surfaces:source.getSnapshot()?.application_surfaces
      const item=candidates?.find(item=>item.bundle===c.bundle&&item.reference.name===c.name)
      if(!item)throw new Error('This extension is unavailable in the current Session')
      if(c.action==='surface')await command({action:'ui_surface',reference:item.reference})
      else await input.command({action:'application_ui_surface',reference:item.reference})
    }else if(c.action==='remote_surface')await command({action:'reopen_remote_surface',bundle:c.bundle,surface:c.name})
    else await command(c)
  }
  const bindHosts=()=>{
    let updated=false
    for(const host of resources.getSnapshot().filter(relevant)){
      if(claimed.current.has(host.entry.view))continue
      let c;try{c=descriptor(host,surface)}catch{continue}
      const match=Object.values(current.current.state.tabs).find(tab=>tab.contentId===JSON.stringify(c)&&bindings.current.get(tab.id)?.requested&&!bindings.current.get(tab.id)?.view)
      if(match){bindings.current.set(match.id,{view:host.entry.view});claimed.current.add(host.entry.view);updated=true}
    }
    return updated
  }
  const reconcile=async(next:DockDocument,valid=()=>alive.current)=>{
    if(!valid())return
    for(const [id,binding] of bindings.current)if(!next.state.tabs[id]){
      bindings.current.delete(id)
      if(!valid())return
      if(binding.view)await input.command({action:'close_detail',view:binding.view})
      // Terminal component unmount owns detach; it never terminates its shell.
    }
    if(!valid())return
    publish(next);bindHosts()
    for(const tab of Object.values(next.state.tabs)){
      if(!valid())return
      const c=coordinate(JSON.parse(tab.contentId));let binding=bindings.current.get(tab.id)
      if(c.action==='resources'||c.action==='terminal')continue
      if(!binding){binding={requested:true};bindings.current.set(tab.id,binding)
        // Command acceptance can precede the presentation frame. Keep the
        // pending binding so adopt() associates that later frame with this tab.
        try{await reopen(c);if(!valid())return;bindHosts()}
        catch(error){if(!valid())return;bindings.current.set(tab.id,{error:String(error)})}
      }
      const view=bindings.current.get(tab.id)?.view
      if(view){const host=resourceHosts.get(view);if(host&&host.entry.floating!==floatingTab(next.state,tab.id))await input.command({action:'float_panel',view,floating:floatingTab(next.state,tab.id)})}
    }
    if(valid())changed(n=>n+1)
  }
  const apply=(operation:string,fields:Record<string,unknown>={})=>enqueue(async valid=>{if(!ready.current)throw new Error('Resource layout is loading');await reconcile(await save({kind:'dock',operation,...fields}),valid)})
  useEffect(()=>{
    alive.current=true;ready.current=false;let live=true,owner:DeviceStore|undefined,unsubscribe:(()=>void)|undefined
    const reload=()=>{if(live)enqueue(async valid=>{if(owner&&live){const saved=await owner.read('layouts',scope);if(live)await reconcile(saved.value as DockDocument,valid)}})}
    enqueue(async valid=>{
      if(identity)try{
        owner=await DeviceStore.open(identity) as DeviceStore
        if(!live||!valid()){owner.close();return}
        store.current=owner
        const saved=await owner.read('layouts',scope)
        if(!live||!valid())return
        publish(saved.value as DockDocument)
      }catch(error){if(!live||!valid())return;setNotice(`Resource layout could not be loaded: ${String(error)}`)}
      if(!live||!valid())return
      ready.current=true
      await reconcile(current.current,valid)
      if(!live||!valid())return
      if(owner){unsubscribe=owner.subscribe('layouts',scope,reload);window.addEventListener('focus',reload)}
      adopt()
    })
    return()=>{live=false;alive.current=false;generation.current++;ready.current=false;unsubscribe?.();window.removeEventListener('focus',reload);owner?.close();store.current=undefined}
  },[identity])
  const adopt=()=>{
    if(!ready.current)return
    for(const view of claimed.current)if(!resourceHosts.has(view))claimed.current.delete(view)
    const rebound=bindHosts()
    for(const host of hostList.current.filter(relevant)){
      if(claimed.current.has(host.entry.view))continue
      claimed.current.add(host.entry.view)
      enqueue(async valid=>{
        try{
          const c=descriptor(host,surface)
          const next=await save({kind:'dock',operation:'open',coordinate:c,title:host.title,duplicate:true})
          if(!valid())return
          const added=getPane(next.state,next.state.activePaneId).activeTabId!
          bindings.current.set(added,{view:host.entry.view});await reconcile(next,valid)
          if(!valid())return
          update({resourcesClosed:false})
        }catch(error){if(valid())await input.command({action:'close_detail',view:host.entry.view});throw error}
      })
    }
    if(rebound)changed(n=>n+1)
  }
  useEffect(adopt,[hosts])
  const openTerminal=(id?:string)=>enqueue(async valid=>{
    let created:Follower|undefined
    try{
      if(!id){created=(await terminal({type:'create',size:{rows:24,columns:80}})).value;id=created!.terminal.id}
      if(!valid()){if(created)await terminal({type:'detach',attachment:created.id});return}
      const previous=current.current,next=await save({kind:'dock',operation:'open',coordinate:{action:'terminal',terminal:id},title:'Terminal'})
      if(!valid()){if(created)await terminal({type:'detach',attachment:created.id});return}
      const tab=Object.values(next.state.tabs).find(tab=>JSON.parse(tab.contentId).terminal===id)!
      if(created&&previous.state.tabs[tab.id])await terminal({type:'detach',attachment:created.id})
      else if(created)bindings.current.set(tab.id,{created})
      await reconcile(next,valid);if(valid())update({resourcesClosed:false})
    }catch(error){if(created)await terminal({type:'detach',attachment:created.id});throw error}
  })
  useEffect(()=>{const show=()=>{if(activeRef.current){apply('open',{coordinate:{action:'resources'},title:'Resources'});update({resourcesClosed:false})}};window.addEventListener('rsi:terminals',show);return()=>window.removeEventListener('rsi:terminals',show)},[])
  const intents:DockIntents={focusTab:tab=>apply('focus',{tab}),focusPane:pane=>apply('focus-pane',{pane}),splitPane:pane=>apply('split',{pane}),addTab:pane=>apply('open',{coordinate:{action:'resources'},title:'Resources',pane,duplicate:true}),closeTab:tab=>apply('close',{tab}),duplicateTab:tab=>apply('duplicate',{tab}),floatTab:(tab,rect)=>apply('float',{tab,rect}),unfloatPane:pane=>apply('dock',{pane}),placeTab:(tab,pane,index)=>apply('place',{tab,pane,index}),dropTab:(tab,pane,zone)=>apply('drop',{tab,pane,zone}),moveFloat:(pane,x,y)=>apply('move-float',{pane,x,y}),resizeFloat:(pane,rect)=>apply('resize-float',{pane,rect}),resizeSplit:(split,sizes)=>apply('resize',{split,sizes})}
  const body=(tab:TabRecord)=>{
    const c=coordinate(JSON.parse(tab.contentId)),binding=bindings.current.get(tab.id)
    if(c.action==='resources')return <div className="resource-launcher">{launcher}<TerminalList active={active&&getPane(doc.state,paneFor(doc.state,tab.id)!).activeTabId===tab.id} list={()=>terminal({type:'list'})} open={openTerminal}/></div>
    if(c.action==='terminal')return <Terminals pane={pane} surface={surface} initial={String(c.terminal)} created={binding?.created} onOpen={openTerminal} hide={()=>apply('close',{tab:tab.id})}/>
    if(binding?.view&&resourceHosts.has(binding.view))return <ResourceBody view={binding.view}/>
    return <div className="resource-unavailable"><p role="status">{binding?.error??(binding?.view?'This resource has retired.':'Opening resource…')}</p><Button onClick={()=>enqueue(async valid=>{bindings.current.delete(tab.id);await reconcile(current.current,valid)})}>Reopen resource</Button></div>
  }
  void revision
  const displayed=useMemo(()=>displayDock(doc.state,viewport.width,viewport.height,narrow),[doc.state,viewport.width,viewport.height,narrow])
  const fullscreen=doc.state.mode==='fullscreen'||narrow
  return <section className={`resource-dock${fullscreen?' resource-fullscreen':''}`} hidden={!active||layout.resourcesClosed} aria-label="Resource workbench" data-session={surface.session} onKeyDown={event=>{
      if(!(event.ctrlKey||event.metaKey)||event.altKey||event.nativeEvent.isComposing||(event.target as HTMLElement).closest('input,textarea,[contenteditable=true]'))return
      if(event.key.toLowerCase()==='z'){event.preventDefault();apply(event.shiftKey?'redo':'undo')}
    }}>
    {notice&&<p className="resource-notice" role="alert">{notice}</p>}
    {!Object.keys(doc.state.tabs).length&&<div className="resource-launcher">{launcher}<TerminalList list={()=>terminal({type:'list'})} open={openTerminal}/></div>}
    <DockLayout state={displayed} active={active&&!layout.resourcesClosed} keepMounted={()=>true} dropZones="horizontal" minPaneFraction={.2} canSplit={!narrow&&dockPaneIds(doc.state).length<2} canAddTab={()=>Object.keys(doc.state.tabs).length<16} intents={intents} labels={labels} renderTabTitle={tab=>{const id=bindings.current.get(tab.id)?.view;return id?resourceHosts.get(id)?.title??tab.title:tab.title}} renderTab={body} renderTabMenuItems={(tab,dismiss)=><><button role="menuitem" onClick={()=>{dismiss();apply('duplicate',{tab:tab.id})}}>Duplicate</button><button role="menuitem" disabled={doc.state.floats.length>=4} onClick={()=>{dismiss();apply('float',{tab:tab.id})}}>Float</button></>} chrome={<><Button size="sm" aria-label="Undo layout" disabled={!doc.history.cursor} onClick={()=>apply('undo')}>↶</Button><Button size="sm" aria-label="Redo layout" disabled={doc.history.cursor===doc.history.entries.length} onClick={()=>apply('redo')}>↷</Button><Button size="sm" aria-label={doc.state.mode==='fullscreen'?'Exit resource fullscreen':'Resource fullscreen'} onClick={()=>apply('mode',{mode:doc.state.mode==='fullscreen'?'push':'fullscreen'})}>⛶</Button><Button size="sm" aria-label="Hide resources" onClick={()=>update({resourcesClosed:true})}>×</Button></>}/>
  </section>
}
function TerminalList({list,open,active=true}:{active?:boolean;list:()=>Promise<{value:{id:string;phase:{state:string}}[]}>;open:(id?:string)=>void}) {
  const [items,setItems]=useState<{id:string;phase:{state:string}}[]>([]),[error,setError]=useState('')
  const refresh=()=>void list().then(reply=>{setItems(reply.value);setError('')},error=>setError(String(error)))
  useEffect(()=>{if(active)refresh()},[active])
  return <section className="terminal-list" aria-label="Terminals"><h2>Terminals</h2><Button onClick={()=>open()}>New terminal</Button><Button onClick={refresh}>Refresh terminals</Button>{items.map((item,index)=><Button key={item.id} title={item.id} onClick={()=>open(item.id)}>Terminal {index+1} · {item.phase.state}</Button>)}{error&&<p role="alert">{error}</p>}</section>
}
