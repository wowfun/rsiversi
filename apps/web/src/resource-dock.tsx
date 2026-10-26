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
import {ResourceOwner} from './resource-owner.mjs'

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
interface Binding {label?:string;view?:string;open_revision?:string;opening?:ReturnType<ResourceOwner['open']>;created?:ReturnType<ResourceOwner['attachment']>;error?:string;floating?:boolean}
export function openTerminals() { window.dispatchEvent(new Event('rsi:terminals')) }
export function ResourceDocks({launcher}:{launcher:ReactNode}) {
  const surfaces=useView(view=>view?.surfaces),selected=useSelected(value=>value)
  const owner=input.resourceOwner()
  if(!owner)return null
  return <>{Object.entries(surfaces??{}).map(([pane,surface])=>surface&&surface.kind!=='external'&&<SessionDock key={`${owner.id}:${pane}:${surface.generation}`} owner={owner} pane={pane} surface={surface} active={pane===selected} launcher={launcher}/>)}</>
}
export function ResourceCleanupNotice() {
  useView(view=>!!view)
  const owner=input.resourceOwner()
  const errors=useSyncExternalStore(owner?.subscribe??(()=>()=>{}),owner?.getSnapshot??(()=>emptyErrors))
  return owner&&errors.length>0&&<div className="resource-cleanup-notice" role="alert" aria-label="Resource cleanup" tabIndex={0}>Resource operations need attention.{errors.map((error:any)=><p key={error.key}><span title={error.key} data-session={error.session}>{error.label}</span>: {error.error} {error.retryable?<Button aria-label={`Retry cleanup for ${error.label} (${error.key})`} disabled={error.busy} onClick={()=>owner.retry(error.key)}>Retry cleanup</Button>:<span>Disconnect and reconnect to resolve this uncertain operation.</span>}</p>)}</div>
}
const emptyErrors:never[]=[]
function SessionDock({owner,pane,surface,active,launcher}:{owner:ResourceOwner;pane:string;surface:Surface;active:boolean;launcher:ReactNode}) {
  const identity=useSyncExternalStore(presentationIdentity.subscribe,presentationIdentity.getSnapshot)
  const hosts=useSyncExternalStore(resources.subscribe,resources.getSnapshot)
  const [doc,setDoc]=useState<DockDocument>(emptyDock),[notice,setNotice]=useState(''),[revision,changed]=useState(0)
  const current=useRef(doc),store=useRef<DeviceStore>(),queue=useRef(Promise.resolve()),alive=useRef(true),ready=useRef(false),generation=useRef(0)
  const bindings=useRef(new Map<TabId,Binding>()),claimed=useRef(new Set<string>()),hostList=useRef(hosts),activeRef=useRef(active)
  const {layout,update}=useLayout(),narrow=useNarrow();activeRef.current=active;hostList.current=hosts
  const viewport=useViewport()
  const scope=`dock:${surface.session}`
  const relevant=(host:ResourceHost)=>host.entry.session===surface.session&&host.entry.pane===pane&&host.entry.generation===surface.generation||host.entry.session===null&&pane==='main'
  const command=(value:Record<string,unknown>)=>owner.call('command',JSON.stringify(value))
  const terminal=async(request:unknown)=>JSON.parse(await owner.call('terminal',JSON.stringify({pane,generation:surface.generation,request})))
  const publish=(next:DockDocument)=>{current.current=next;if(alive.current)setDoc(next)}
  const enqueue=(work:(valid:()=>boolean)=>Promise<void>,retired=()=>{})=>{const epoch=generation.current,valid=()=>alive.current&&generation.current===epoch;queue.current=queue.current.then(async()=>{if(valid())await work(valid);else retired()}).catch(error=>{if(valid())setNotice(String(error))})}
  const save=async(intent:DockIntent)=>{
    const storage=store.current
    const reserved:ReturnType<ResourceOwner['reservePanel']>[]=[]
    // A failed save is reported and the previously committed layout is retained.
    try{
      let next:DockDocument
      if(storage)next=(await storage.apply('layouts',intent,scope,(values:unknown[])=>reserved.push(...reserveRemoved(values[0] as DockDocument)))).value as DockDocument
      else {setNotice('Resource layout is not saved on this device.');next=dockIntent(current.current,intent);reserved.push(...reserveRemoved(next))}
      for(const item of reserved)item.commit()
      return next
    }catch(error){for(const item of reserved)item.cancel();throw error}
  }
  const openingCommand=(c:Coordinate)=>{
    if(c.action==='surface'||c.action==='application_surface'){
      const candidates=c.action==='surface'?source.getSnapshot()?.surfaces[pane]?.ui_surfaces:source.getSnapshot()?.application_surfaces
      const item=candidates?.find(item=>item.bundle===c.bundle&&item.reference.name===c.name)
      if(!item)throw new Error('This extension is unavailable in the current Session')
      if(c.action==='application_surface')return {action:'application_ui_surface',reference:item.reference}
      return {action:'ui_surface',reference:item.reference,pane,generation:surface.generation}
    }
    return {...(c.action==='remote_surface'?{action:'reopen_remote_surface',bundle:c.bundle,surface:c.name}:c),pane,generation:surface.generation}
  }
  const bindHosts=()=>{
    let updated=false
    for(const host of resources.getSnapshot().filter(relevant)){
      if(owner.suppresses(host.entry))continue
      for(const binding of bindings.current.values())if(binding.opening?.request===host.entry.opening_request || binding.view===host.entry.view&&binding.open_revision!==host.entry.open_revision){
        // A new explicit inspection reuses this still-open tab, never a closed one.
        binding.view=host.entry.view;binding.open_revision=host.entry.open_revision;binding.opening=undefined;binding.error=undefined
        claimed.current.add(`${host.entry.view}:${host.entry.open_revision}`);updated=true
      }
    }
    return updated
  }
  const release=(binding:Binding)=>binding.created?{commit:binding.created.close,cancel:()=>{}}:binding.opening?owner.reserveOpen(binding.opening):binding.view?owner.reservePanel(surface.session,{view:binding.view,open_revision:binding.open_revision},binding.label):undefined
  const reserveRemoved=(next:DockDocument)=>{
    const reservations:ReturnType<ResourceOwner['reservePanel']>[]=[]
    try{for(const [id,binding] of bindings.current)if(!next.state.tabs[id]){const reserved=release(binding);if(reserved)reservations.push(reserved)}}
    catch(error){for(const reserved of reservations)reserved.cancel();throw error}
    return reservations
  }
  const reconcile=async(next:DockDocument,valid=()=>alive.current)=>{
    if(!valid())return
    for(const [id,binding] of bindings.current)if(!next.state.tabs[id]){
      release(binding)?.commit()
      bindings.current.delete(id)
      // Terminal component unmount owns detach; it never terminates its shell.
    }
    if(!valid())return
    publish(next);bindHosts()
    for(const tab of Object.values(next.state.tabs)){
      if(!valid())return
      const c=coordinate(JSON.parse(tab.contentId));let binding=bindings.current.get(tab.id)
      if(c.action==='resources'||c.action==='terminal')continue
      if(!binding){
        binding={label:tab.title};bindings.current.set(tab.id,binding)
        try{
          const opening=owner.open(surface.session,openingCommand(c),tab.title),selected=binding
          selected.opening=opening
          void opening.promise!.then((reply:any)=>{
            if(!valid()||bindings.current.get(tab.id)!==selected){owner.reserveOpen(opening).commit();return}
            if(reply.status==='unavailable'){selected.error=reply.reason;selected.opening=undefined}
            else {selected.opening=undefined;selected.error=undefined;selected.view=reply.view;selected.open_revision=reply.open_revision;claimed.current.add(`${reply.view}:${reply.open_revision}`)}
            changed(n=>n+1)
          }).catch((error:unknown)=>{if(valid()&&bindings.current.get(tab.id)===selected){selected.error=String(error);changed(n=>n+1)}})
        }catch(error){binding.error=String(error)}
      }
      const view=bindings.current.get(tab.id)?.view
      if(view){const host=resourceHosts.get(view),floating=floatingTab(next.state,tab.id),selected=bindings.current.get(tab.id)!
        if(host&&host.entry.floating!==floating&&selected.floating!==floating){selected.floating=floating;void command({action:'float_panel',view,floating}).catch((error:unknown)=>{selected.floating=undefined;if(valid())setNotice(String(error))})}}
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
    return()=>{live=false;alive.current=false;generation.current++;ready.current=false;for(const binding of bindings.current.values())release(binding)?.commit();bindings.current.clear();unsubscribe?.();window.removeEventListener('focus',reload);owner?.close();store.current=undefined}
  },[identity])
  const adopt=()=>{
    if(!ready.current)return
    for(const key of claimed.current){const [view,revision]=key.split(':');if(resourceHosts.get(view)?.entry.open_revision!==revision)claimed.current.delete(key)}
    const rebound=bindHosts()
    for(const host of hostList.current.filter(relevant)){
      const key=`${host.entry.view}:${host.entry.open_revision}`
      if(claimed.current.has(key)||owner.suppresses(host.entry)||owner.owns(host.entry.opening_request))continue
      claimed.current.add(key)
      enqueue(async valid=>{
        try{
          const c=descriptor(host,surface)
          const next=await save({kind:'dock',operation:'open',coordinate:c,title:host.title,duplicate:true})
          if(!valid())return
          const added=getPane(next.state,next.state.activePaneId).activeTabId!
          bindings.current.set(added,{label:host.title,view:host.entry.view,open_revision:host.entry.open_revision});await reconcile(next,valid)
          if(!valid())return
          update({resourcesClosed:false})
        }catch(error){owner.reservePanel(surface.session,host.entry,host.title).commit();throw error}
      })
    }
    if(rebound)changed(n=>n+1)
  }
  useEffect(adopt,[hosts])
  const openTerminal=(id?:string)=>{
    const epoch=generation.current,valid=()=>alive.current&&generation.current===epoch
    let created:ReturnType<ResourceOwner['attachment']>|undefined
    try{if(!id)created=owner.attachment(surface.session,pane,surface.generation,{type:'create',size:{rows:24,columns:80}})}catch(error){setNotice(String(error));return}
    // Creating a shell may take time; only persistence belongs in the layout queue.
    void (created?created.promise.then((value:Follower)=>value.terminal.id):Promise.resolve(id!)).then(terminalId=>{
      if(!valid()){created?.close();return}
      enqueue(async currentValid=>{
        try{
          if(!valid()||!currentValid()){created?.close();return}
          const previous=current.current,next=await save({kind:'dock',operation:'open',coordinate:{action:'terminal',terminal:terminalId},title:'Terminal'})
          if(!valid()||!currentValid()){created?.close();return}
          const tab=Object.values(next.state.tabs).find(tab=>JSON.parse(tab.contentId).terminal===terminalId)!
          if(created&&previous.state.tabs[tab.id])created.close()
          else if(created)bindings.current.set(tab.id,{created})
          await reconcile(next,currentValid);if(valid())update({resourcesClosed:false})
        }catch(error){created?.close();throw error}
      },()=>created?.close())
    }).catch(error=>{created?.close();if(valid())setNotice(String(error))})
  }
  useEffect(()=>{const show=()=>{if(activeRef.current){apply('open',{coordinate:{action:'resources'},title:'Resources'});update({resourcesClosed:false})}};window.addEventListener('rsi:terminals',show);return()=>window.removeEventListener('rsi:terminals',show)},[])
  const intents:DockIntents={focusTab:tab=>apply('focus',{tab}),focusPane:pane=>apply('focus-pane',{pane}),splitPane:pane=>apply('split',{pane}),addTab:pane=>apply('open',{coordinate:{action:'resources'},title:'Resources',pane,duplicate:true}),closeTab:tab=>apply('close',{tab}),duplicateTab:tab=>apply('duplicate',{tab}),floatTab:(tab,rect)=>apply('float',{tab,rect}),unfloatPane:pane=>apply('dock',{pane}),placeTab:(tab,pane,index)=>apply('place',{tab,pane,index}),dropTab:(tab,pane,zone)=>apply('drop',{tab,pane,zone}),moveFloat:(pane,x,y)=>apply('move-float',{pane,x,y}),resizeFloat:(pane,rect)=>apply('resize-float',{pane,rect}),resizeSplit:(split,sizes)=>apply('resize',{split,sizes})}
  const body=(tab:TabRecord)=>{
    const c=coordinate(JSON.parse(tab.contentId)),binding=bindings.current.get(tab.id)
    if(c.action==='resources')return <div className="resource-launcher">{launcher}<TerminalList active={active&&getPane(doc.state,paneFor(doc.state,tab.id)!).activeTabId===tab.id} list={()=>terminal({type:'list'})} open={openTerminal}/></div>
    if(c.action==='terminal')return <Terminals owner={owner} pane={pane} surface={surface} initial={String(c.terminal)} created={binding?.created} onOpen={openTerminal} hide={()=>apply('close',{tab:tab.id})}/>
    if(binding?.view&&resourceHosts.has(binding.view))return <ResourceBody view={binding.view}/>
    return <div className="resource-unavailable"><p role="status">{binding?.error??(binding?.view?'This resource has retired.':'Opening resource…')}</p><Button disabled={!binding?.error&&!binding?.view} onClick={()=>enqueue(async valid=>{const old=bindings.current.get(tab.id);if(old)release(old)?.commit();bindings.current.delete(tab.id);await reconcile(current.current,valid)})}>Reopen resource</Button></div>
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
