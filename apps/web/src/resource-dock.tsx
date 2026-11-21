import type {NativeSurface} from './presentation-types.ts';
import css from "./resources.module.css"
import {useCallback,useEffect,useLayoutEffect,useMemo,useRef,useState,useSyncExternalStore} from 'react'
import {IconFullscreenOutlineRegular,IconPanelLeftOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx'
import {DockLayout} from '../vendor/dsh/dockkit/components/DockSurface.tsx'
import type {DockIntents,DockLabels} from '../vendor/dsh/dockkit/contract/adapter.ts'
import type {TabId,TabRecord} from '../vendor/dsh/dockkit/contract/types.ts'
import {dockIntent,dockStorageKey,floatingTab,dockPaneIds,displayDock,guideContentId,guideInPane,paneFor,getPane,type DockDocument,type DockIntent,type Coordinate} from './dock-state.ts'
import {composerInteraction,resources,resourceHosts,type ResourceHost} from './resource-hosts.ts'
import {input,source,useSelected,useView} from './bridge.ts'
import {activeResourceDock,openDockWindow,presentationIdentity,useLayout,type Viewport} from './presentation.tsx'
import {Terminals,type Follower} from './terminal.tsx'
import {Button} from './button.tsx'
import {ExternalResources, ResourceGuide} from './resources.tsx'
import {ResourceOwner} from './resource-owner.mjs'
import {ScheduledTaskDetail} from './session/scheduled-tasks.tsx'
import {FileImagePanel} from './session/transcript-file-image.tsx'
import {DocumentFilePanel, type DocumentLocation} from './session/document-file.tsx'

const labels:DockLabels={emptyPane:'Open a resource to use this pane',splitPane:'Split resources',splitPaneDisabled:'Two resource panes are open',splitPaneNarrow:'Widen resources to split',closeTab:'Close resource tab',addTab:'Open resources',dockFloat:'Dock resource',closeFloat:'Close resource window',resizePane:'Resize resource panes',moveFloat:'Move resource window',resizeFloat:'Resize resource window',describeFloatPosition:(x,y)=>`Position ${x}, ${y} pixels. Arrow keys move; Shift changes by 50 pixels. Home and End select viewport corners.`,describeFloatSize:(w,h)=>`${w} by ${h} pixels. Arrow keys resize; Shift changes by 50 pixels. Home selects minimum size; End fills available space.`,dropZone:{center:'Merge tabs',left:'Split left',right:'Split right',top:'',bottom:''}}
const descriptor=(host:ResourceHost,surface:NativeSurface)=>{
  const value=host.entry.reopen
  if(!value)throw new Error('This resource cannot be reopened')
  if(value.action==='ui_surface'||value.action==='application_ui_surface'){
    const candidates=value.action==='ui_surface'?surface.ui_surfaces:source.getSnapshot()?.application_surfaces??[]
    const entry=candidates.find(item=>JSON.stringify(item.reference)===JSON.stringify(value.reference))
    if(!entry)throw new Error('This extension has retired')
    return {action:value.action==='ui_surface'?'surface':'application_surface',bundle:entry.bundle,name:entry.reference.name}
  }
  const {pane:_,generation:__,...saved}=value
  return saved
}
interface Binding {label?:string;view?:string;open_revision?:string;opening?:ReturnType<ResourceOwner['open']>;created?:ReturnType<ResourceOwner['attachment']>;error?:string;floating?:boolean}
export function openTerminals() { window.dispatchEvent(new Event('rsi:terminals')) }
export function ResourceDocks({viewport}:{viewport:Viewport}) {
  const surfaces=useView(view=>view?.surfaces),selected=useSelected(value=>value)
  const owner=input.resourceOwner()
  const identity=useSyncExternalStore(presentationIdentity.subscribe,presentationIdentity.getSnapshot)
  const {layout}=useLayout()
  const active=surfaces?.[selected]
  const fallback=!owner||!active||active.kind==='external'
  return <>{fallback&&!layout.resourcesClosed&&active&&<section className={`${css.scope} resource-dock`} aria-label="Resource workbench">{active.kind==='external'?<ExternalResources surface={active}/>:<ResourceGuide key={active.generation} surface={active} pane={selected}/>}</section>}{owner&&Object.entries(surfaces??{}).map(([pane,surface])=>surface&&surface.kind!=='external'&&<SessionDock key={JSON.stringify([owner.id,pane,surface.generation,identity])} identity={identity} owner={owner} pane={pane} surface={surface} active={pane===selected} viewport={viewport}/>)}</>
}
export function ResourceCleanupNotice() {
  useView(view=>!!view)
  const owner=input.resourceOwner()
  const errors=useSyncExternalStore(owner?.subscribe??(()=>()=>{}),owner?.getSnapshot??(()=>emptyErrors))
  return owner&&errors.length>0&&<div className="resource-cleanup-notice" role="alert" aria-label="Resource cleanup" tabIndex={0}>Resource operations need attention.{errors.map((error:any)=><p key={error.key}><span title={error.key} data-session={error.session}>{error.label}</span>: {error.error} {error.retryable?<Button aria-label={`Retry cleanup for ${error.label} (${error.key})`} aria-disabled={error.busy} onClick={()=>owner.retry(error.key)}>Retry cleanup</Button>:<span>Disconnect and reconnect to resolve this uncertain operation.</span>}</p>)}</div>
}
const emptyErrors:never[]=[]
function ResourceBody({view}:{view:string}) {
  const root=useRef<HTMLDivElement>(null)
  useLayoutEffect(()=>{
    const host=resourceHosts.get(view)
    if(!host||!root.current)return
    root.current.append(host.body)
    return ()=>{if(host.body.isConnected)document.getElementById('resource-parking')?.append(host.body)}
  },[view])
  return <div className="resource-host" ref={root}/>
}
function SessionDock({owner,pane,surface,active,viewport,identity}:{identity?:string;owner:ResourceOwner;pane:string;surface:NativeSurface;active:boolean;viewport:Viewport}) {
  const [dock]=useState(()=>openDockWindow(identity?dockStorageKey(identity,surface.session):undefined))
  const doc=useSyncExternalStore(dock.subscribe,dock.getSnapshot)
  useLayoutEffect(()=>{
    if(!active)return
    activeResourceDock.set(dock)
    return ()=>{if(activeResourceDock.getSnapshot()===dock)activeResourceDock.set(undefined)}
  },[dock,active])
  const hosts=useSyncExternalStore(resources.subscribe,resources.getSnapshot)
  const [notice,setNotice]=useState(''),[revision,changed]=useState(0)
  const [documentLocation,setDocumentLocation]=useState<DocumentLocation>()
  const alive=useRef(true),generation=useRef(0)
  const bindings=useRef(new Map<TabId,Binding>()),claimed=useRef(new Set<string>()),activeRef=useRef(active)
  const narrow=viewport.width<768;activeRef.current=active
  const relevant=(host:ResourceHost)=>host.entry.session===surface.session&&host.entry.pane===pane&&host.entry.generation===surface.generation||host.entry.session===null&&pane==='main'
  const command=(value:Record<string,unknown>)=>owner.call('command',JSON.stringify(value))
  const terminal=async(request:unknown)=>JSON.parse(await owner.call('terminal',JSON.stringify({pane,generation:surface.generation,request})))
  const openingCommand=(c:Coordinate)=>{
    if(c.action==='surface'||c.action==='application_surface'){
      const currentSurface=source.getSnapshot()?.surfaces[pane]
      const candidates=c.action==='surface'?(currentSurface?.kind==='native'?currentSurface.ui_surfaces:undefined):source.getSnapshot()?.application_surfaces
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
      if(composerInteraction(host)||owner.suppresses(host.entry))continue
      for(const binding of bindings.current.values())if(binding.opening?.request===host.entry.opening_request || binding.view===host.entry.view&&binding.open_revision!==host.entry.open_revision){
        // A new explicit inspection reuses this still-open tab, never a closed one.
        binding.view=host.entry.view;binding.open_revision=host.entry.open_revision;binding.opening=undefined;binding.error=undefined
        claimed.current.add(`${host.entry.view}:${host.entry.open_revision}`);updated=true
      }
    }
    return updated
  }
  const release=(binding:Binding)=>binding.created?{commit:binding.created.close,cancel:()=>{}}:binding.opening?owner.reserveOpen(binding.opening):binding.view?owner.reservePanel(surface.session,{view:binding.view,open_revision:binding.open_revision},binding.label):undefined
  const reconcile=(next:DockDocument,attach?:(next:DockDocument)=>void)=>{
    const epoch=generation.current,valid=()=>alive.current&&generation.current===epoch
    const removed:Array<[TabId,ReturnType<typeof release>]>=[]
    try{
      for(const [id,binding] of bindings.current)if(!next.state.tabs[id])removed.push([id,release(binding)])
    }catch(error){for(const [,item] of removed)item?.cancel();throw error}
    for(const [id,item] of removed){bindings.current.delete(id);item?.commit()}
    attach?.(next)
    const previous=dock.getSnapshot()
    dock.commit(next);bindHosts()
    for(const tab of Object.values(next.state.tabs)){
      const c=JSON.parse(tab.contentId) as Coordinate;let binding=bindings.current.get(tab.id)
      if(c.action==='resources'||c.action==='terminal'||c.action==='schedule_task'||c.action==='file_image'||c.action==='workspace_document')continue
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
    if(next===previous)changed(n=>n+1)
  }
  const change=(intent:DockIntent,attach?:(next:DockDocument)=>void)=>{
    const previous=dock.getSnapshot(),next=dockIntent(previous,intent)
    if(next!==previous || attach)reconcile(next,attach)
  }
  const apply=(operation:string,fields:Record<string,unknown>={})=>{
    try{change({kind:'dock',operation,...fields});setNotice('')}
    catch(error){setNotice(String(error))}
  }
  useEffect(()=>{
    alive.current=true
    reconcile(dock.getSnapshot())
    return ()=>{
      alive.current=false;generation.current++
      for(const binding of bindings.current.values())release(binding)?.commit()
      bindings.current.clear();claimed.current.clear()
    }
  },[])
  const adopt=()=>{
    for(const key of claimed.current){const [view,revision]=key.split(':');if(resourceHosts.get(view)?.entry.open_revision!==revision)claimed.current.delete(key)}
    const rebound=bindHosts()
    for(const host of hosts.filter(relevant)){
      if(composerInteraction(host))continue
      const key=`${host.entry.view}:${host.entry.open_revision}`
      if(claimed.current.has(key)||owner.suppresses(host.entry)||owner.owns(host.entry.opening_request))continue
      claimed.current.add(key)
      try{
        const c=descriptor(host,surface)
        change({kind:'dock',operation:'open',coordinate:c,title:host.title,duplicate:true},next=>{
          const added=getPane(next.state,next.state.activePaneId).activeTabId!
          bindings.current.set(added,{label:host.title,view:host.entry.view,open_revision:host.entry.open_revision})
        })
      }catch(error){owner.reservePanel(surface.session,host.entry,host.title).commit();setNotice(String(error))}
    }
    if(rebound)changed(n=>n+1)
  }
  useEffect(adopt,[hosts])
  const openTerminal=(id?:string)=>{
    const epoch=generation.current,valid=()=>alive.current&&generation.current===epoch
    let created:ReturnType<ResourceOwner['attachment']>|undefined
    try{if(!id)created=owner.attachment(surface.session,pane,surface.generation,{type:'create',size:{rows:24,columns:80}})}catch(error){setNotice(String(error));return}
    void (created?created.promise.then((value:Follower)=>value.terminal.id):Promise.resolve(id!)).then(terminalId=>{
      if(!valid()){created?.close();return}
      const previous=dock.getSnapshot()
      change({kind:'dock',operation:'open',coordinate:{action:'terminal',terminal:terminalId},title:'Terminal'},next=>{
        const tab=Object.values(next.state.tabs).find(tab=>JSON.parse(tab.contentId).terminal===terminalId)!
        if(created&&previous.state.tabs[tab.id])created.close()
        else if(created)bindings.current.set(tab.id,{created})
      })
    }).catch(error=>{created?.close();if(valid())setNotice(String(error))})
  }
  useEffect(()=>{const show=()=>{if(activeRef.current)apply('open',{coordinate:{action:'resources'},title:'Resources'})};window.addEventListener('rsi:terminals',show);return()=>window.removeEventListener('rsi:terminals',show)},[])
  useEffect(()=>{
    const show=(event:Event)=>{
      const target=(event as CustomEvent).detail
      if(!activeRef.current||target?.pane!==pane||target.generation!==surface.generation)return
      if(event.type==='rsi:file-image'||event.type==='rsi:workspace-document'){
        const document=event.type==='rsi:workspace-document'
        if(document)setDocumentLocation({path:target.path,line:target.line,details:target.details})
        apply('open',{coordinate:{action:document?'workspace_document':'file_image',path:target.path},title:target.path.split(/[\\/]/).at(-1)})
      }
      else apply('open',{coordinate:{action:'schedule_task',task:target.task},title:'Automation task'})
    }
    window.addEventListener('rsi:schedule-detail',show)
    window.addEventListener('rsi:file-image',show)
    window.addEventListener('rsi:workspace-document',show)
    return ()=>{window.removeEventListener('rsi:schedule-detail',show);window.removeEventListener('rsi:file-image',show);window.removeEventListener('rsi:workspace-document',show)}
  },[pane,surface.generation])
  const intents:DockIntents={focusTab:tab=>apply('focus',{tab}),focusPane:pane=>apply('focus-pane',{pane}),splitPane:pane=>apply('split',{pane}),addTab:pane=>apply('open',{coordinate:{action:'resources'},title:'Resources',pane}),closeTab:tab=>apply('close',{tab}),duplicateTab:tab=>apply('duplicate',{tab}),floatTab:(tab,rect)=>apply('float',{tab,rect}),unfloatPane:pane=>apply('dock',{pane}),placeTab:(tab,pane,index)=>apply('place',{tab,pane,index}),dropTab:(tab,pane,zone)=>apply('drop',{tab,pane,zone}),moveFloat:(pane,x,y)=>apply('move-float',{pane,x,y}),resizeFloat:(pane,rect)=>apply('resize-float',{pane,rect}),resizeSplit:(split,sizes)=>apply('resize',{split,sizes})}
  const displayed=useMemo(()=>displayDock(doc.state,viewport.width,viewport.height,narrow),[doc.state,viewport.width,viewport.height,narrow])
  const visible=active&&doc.state.expanded
  const list=useCallback(()=>terminal({type:'list'}),[owner,pane,surface.generation])
  const guide=<ResourceGuide surface={surface} pane={pane} terminals={{open:openTerminal,list}}/>
  const body=(tab:TabRecord)=>{
    const c=JSON.parse(tab.contentId) as Coordinate,binding=bindings.current.get(tab.id)
    if(c.action==='schedule_task')return <ScheduledTaskDetail surface={surface} id={String(c.task)}/>
    if(c.action==='workspace_document')return visible&&getPane(displayed,paneFor(displayed,tab.id)!).activeTabId===tab.id?
      <DocumentFilePanel key={tab.contentId} pane={pane} generation={surface.generation} path={String(c.path)} location={documentLocation?.path===c.path?documentLocation:undefined}/>:null
    if(c.action==='file_image')return visible&&getPane(displayed,paneFor(displayed,tab.id)!).activeTabId===tab.id?
      <FileImagePanel pane={pane} generation={surface.generation} path={String(c.path)}/>:null
    if(c.action==='resources')return visible&&getPane(displayed,paneFor(displayed,tab.id)!).activeTabId===tab.id?guide:null
    if(c.action==='terminal')return <Terminals owner={owner} pane={pane} surface={surface} selected={visible&&getPane(displayed,displayed.activePaneId).activeTabId===tab.id} initial={String(c.terminal)} created={binding?.created} onOpen={openTerminal} hide={()=>apply('close',{tab:tab.id})}/>
    if(binding?.view&&resourceHosts.has(binding.view))return <ResourceBody view={binding.view}/>
    return <div className="resource-unavailable"><p role="status">{binding?.error??(binding?.view?'This resource has retired.':'Opening resource…')}</p><Button disabled={!binding?.error&&!binding?.view} onClick={()=>{try{const old=bindings.current.get(tab.id);if(old)release(old)?.commit();bindings.current.delete(tab.id);reconcile(dock.getSnapshot())}catch(error){setNotice(String(error))}}}>Reopen resource</Button></div>
  }
  void revision
  const fullscreen=doc.state.mode==='fullscreen'||narrow
  const chrome=<>
    <Button size="sm" aria-label="Undo layout" disabled={!doc.history.cursor} onClick={()=>apply('undo')}>↶</Button>
    <Button size="sm" aria-label="Redo layout" disabled={doc.history.cursor===doc.history.entries.length} onClick={()=>apply('redo')}>↷</Button>
    <button type="button" className={css.dockAction} aria-label={fullscreen?'Exit resource fullscreen':'Resource fullscreen'} onClick={()=>apply('toggle-fullscreen',{automatic:narrow})}>
      {fullscreen?<svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><path d="M9 2.5V6a1 1 0 0 0 1 1h3.5M7 13.5V10a1 1 0 0 0-1-1H2.5"/></svg>:<IconFullscreenOutlineRegular size={15}/>}
    </button>
    <button type="button" className={css.dockAction} aria-label="Hide resources" onClick={()=>dock.expand(false)}><IconPanelLeftOutlineRegular size={15} className={css.collapseGlyph}/></button>
  </>
  return <section className={`${css.scope} resource-dock${fullscreen?' resource-fullscreen':''}`} hidden={!visible} aria-label="Resource workbench" data-session={surface.session} onKeyDown={event=>{
      if(!(event.ctrlKey||event.metaKey)||event.altKey||event.nativeEvent.isComposing||(event.target as HTMLElement).closest('input,textarea,[contenteditable=true]'))return
      if(event.key.toLowerCase()==='z'){event.preventDefault();apply(event.shiftKey?'redo':'undo')}
    }}>
    {notice&&<p className="resource-notice" role="alert">{notice}</p>}
    {visible&&!Object.keys(doc.state.tabs).length&&guide}
    <DockLayout state={displayed} active={visible} keepMounted={()=>true} dropZones="horizontal" minPaneFraction={.2} canSplit={!narrow&&dockPaneIds(doc.state).length<2} canAddTab={paneId=>Object.keys(doc.state.tabs).length<16&&!guideInPane(displayed,paneId)} intents={intents} labels={labels} renderTabTitle={tab=>{const id=bindings.current.get(tab.id)?.view;return id?resourceHosts.get(id)?.title??tab.title:tab.title}} renderTab={body} renderTabMenuItems={(tab,dismiss)=><>{tab.contentId!==guideContentId&&<button role="menuitem" onClick={()=>{dismiss();apply('duplicate',{tab:tab.id})}}>Duplicate</button>}<button role="menuitem" disabled={doc.state.floats.length>=4} onClick={()=>{dismiss();apply('float',{tab:tab.id})}}>Float</button></>} chrome={chrome}/>
  </section>
}
