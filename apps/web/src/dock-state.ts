// RSI policy around the pinned, host-independent DSH operation engine.
import {createInitialState,createIdMinter} from '../vendor/dsh/dockkit/engine/initial.ts'
import {applyOp} from '../vendor/dsh/dockkit/engine/operations.ts'
import * as plan from '../vendor/dsh/dockkit/engine/planner.ts'
import {record,stepBack,stepForward,EMPTY_HISTORY,type History} from '../vendor/dsh/dockkit/engine/sequence.ts'
import {dockPaneIds,findTabPane,getPane} from '../vendor/dsh/dockkit/engine/tree.ts'
import type {LayoutState,LayoutOp,TabId,PaneId,SplitId,FloatRect} from '../vendor/dsh/dockkit/contract/types.ts'

export type Coordinate = {action:string;[key:string]:unknown}
export interface DockSnapshot {kind:'session-dock';version:2;counter:number;state:LayoutState}
export interface DockDocument extends DockSnapshot {history:History}
export type DockIntent = {kind:'dock';operation:string;[key:string]:unknown}
export const guideContentId=JSON.stringify({action:'resources'})
export function guideInPane(state:LayoutState,pane:PaneId) {
  return plan.findPaneContentTab(state,pane,guideContentId)
}
function focusTab(state:LayoutState,tab:TabId):readonly LayoutOp[] {
  const pane=findTabPane(state,tab)
  return pane.id===state.activePaneId&&pane.activeTabId===tab ? [] : [{type:'focusTab',tabId:tab}]
}
function mergeArrivingGuide(state:LayoutState,tab:TabId,pane:PaneId):readonly LayoutOp[]|undefined {
  if(state.tabs[tab]?.contentId!==guideContentId||getPane(state,pane).host!=='dock')return
  const existing=guideInPane(state,pane)
  if(existing&&existing!==tab)return [{type:'closeTab',tabId:tab},{type:'focusTab',tabId:existing}]
}
const plain=(v:unknown):v is Record<string,any>=>!!v&&typeof v==='object'&&!Array.isArray(v)
const keys=(v:unknown,list:string)=>plain(v)&&Object.keys(v).sort().join()===list.split(',').sort().join()
const text=(v:unknown,max=256):v is string=>typeof v==='string'&&v.length>0&&new TextEncoder().encode(v).length<=max&&!/[\x00-\x1f\x7f]/.test(v)
const id=(v:unknown):v is string=>text(v,80)&&/^(pane|split|tab|float)[1-9][0-9]{0,14}$/.test(v)
const fail=():never=>{throw new Error('Invalid resource layout')}
const floatPixel=(v:unknown):v is number=>typeof v==='number'&&Number.isFinite(v)&&Math.abs(v)<=100000
function checkedFloatRect(value:unknown):FloatRect {
  if(!plain(value)||!keys(value,'x,y,width,height')||!Object.values(value).every(floatPixel)||value.width<100||value.height<100) return fail()
  return value as FloatRect
}
export function coordinate(value:unknown):Coordinate {
  if(!plain(value)||!text(value.action,40)||JSON.stringify(value).length>16384)fail()
  const v=value as Record<string,any>
  const valid=v.action==='terminal' ? keys(v,'action,terminal')&&text(v.terminal) :
    v.action==='resources' ? keys(v,'action') :
    v.action==='schedule_task' ? keys(v,'action,task')&&text(v.task) :
    (v.action==='file_image'||v.action==='workspace_document') ? keys(v,'action,path')&&text(v.path,16*1024) :
    ['surface','application_surface','remote_surface'].includes(v.action) ? keys(v,'action,bundle,name')&&text(v.bundle,128)&&text(v.name,128) :
    ['ui_block','inspect_block'].includes(v.action) ? keys(v,'action,key')&&text(v.key,512) :
    v.action==='inspect_image' ? keys(v,'action,media')&&plain(v.media) :
    v.action==='inspect_source' ? keys(v,'action,source')&&plain(v.source) :
    v.action==='inspect_interaction' ? keys(v,'action,owner,id')&&text(v.owner)&&text(v.id) :
    v.action==='remote_ui_list' ? keys(v,'action') : false
  if(!valid)fail()
  // MediaRef and source coordinates receive their authoritative typed checks in
  // Rust before any read. Never retain a presentation/action/terminal attachment.
  return v as Coordinate
}
export function emptyDock():DockDocument {
  const minter=createIdMinter()
  return {kind:'session-dock',version:2,counter:1,state:createInitialState(minter),history:EMPTY_HISTORY}
}
export function savedDock({counter,state}:DockSnapshot):DockSnapshot {
  return {kind:'session-dock',version:2,counter,state}
}
export function validateDock(value:unknown):DockSnapshot {
  if(!plain(value)||!keys(value,'kind,version,counter,state'))fail()
  const doc=value as DockSnapshot
  if(doc.kind!=='session-dock'||doc.version!==2||!Number.isSafeInteger(doc.counter)||doc.counter<1||doc.counter>1e14)fail()
  validateState(doc.state,doc.counter)
  return savedDock(doc)
}
export function restoreDock(value:unknown):DockDocument {
  const saved=validateDock(value)
  return {...saved,history:EMPTY_HISTORY}
}
export function dockStorageKey(identity:string,session:string) {
  return 'rsi.dock.v1.'+JSON.stringify([identity,session])
}
const maximumSnapshotLength=32*1024
export function readDock(key:string|undefined):DockDocument {
  if(key)try {
    const raw=localStorage.getItem(key)
    if(raw && raw.length<=maximumSnapshotLength)return restoreDock(JSON.parse(raw))
  }catch { /* A missing or unusable snapshot starts a fresh window. */ }
  return emptyDock()
}
export function writeDock(key:string|undefined,doc:DockDocument) {
  if(key)try {
    const raw=JSON.stringify(savedDock(doc))
    if(raw.length<=maximumSnapshotLength)localStorage.setItem(key,raw)
  }catch { /* The live window retains its layout when storage is unavailable. */ }
}
function validateState(state:LayoutState,counter:number) {
  if(!keys(state,'nodes,tabs,rootId,floats,activePaneId,expanded,mode')||!plain(state.nodes)||!plain(state.tabs)||Object.keys(state.nodes).length>7||Object.keys(state.tabs).length>16||!Array.isArray(state.floats)||state.floats.length>4||new Set(state.floats).size!==state.floats.length||typeof state.expanded!=='boolean'||!['push','fullscreen'].includes(state.mode))fail()
  const seen=new Set<string>(),tabs=new Set<string>();let docked=0
  const visit=(nodeId:string,floating:boolean)=>{
    if(!id(nodeId)||Number(nodeId.replace(/^[a-z]+/,''))>counter||seen.has(nodeId))fail();seen.add(nodeId)
    const node=state.nodes[nodeId as PaneId];if(!node||node.id!==nodeId)fail()
    if(node.kind==='split'){
      if(floating||!keys(node,'kind,id,axis,children,sizes')||node.axis!=='row'||!Array.isArray(node.children)||node.children.length!==2||!Array.isArray(node.sizes)||node.sizes.length!==2||node.sizes.some(n=>!Number.isFinite(n)||n<.2||n>.8)||Math.abs(node.sizes.reduce((a,b)=>a+b,0)-1)>1e-8)fail()
      node.children.forEach(child=>visit(child,false))
    }else if(node.kind==='pane'){
      // JSON removes the two optional fields. Accept exactly that normalized form.
      if(Object.keys(node).some(k=>!['kind','id','host','tabs','activeTabId','rect'].includes(k))||node.host!==(floating?'float':'dock')||!Array.isArray(node.tabs)||node.tabs.length>16||(floating&&node.tabs.length!==1))fail()
      if(!floating)docked++
      if(floating)checkedFloatRect(node.rect);else if(node.rect!==undefined)fail()
      if(node.tabs.length?!node.tabs.includes(node.activeTabId!):node.activeTabId!==undefined)fail()
      for(const tabId of node.tabs){if(!id(tabId)||Number(tabId.replace(/^[a-z]+/,''))>counter||tabs.has(tabId))fail();tabs.add(tabId);const tab=state.tabs[tabId];if(!keys(tab,'id,kind,contentId,title')||tab.id!==tabId||tab.kind!=='rsi'||!text(tab.title,512))fail();coordinate(JSON.parse(tab.contentId))}
    }else fail()
  }
  visit(state.rootId,false);state.floats.forEach(float=>visit(float,true))
  if(docked>2||seen.size!==Object.keys(state.nodes).length||tabs.size!==Object.keys(state.tabs).length||!seen.has(state.activePaneId)||state.nodes[state.activePaneId]?.kind!=='pane')fail()
}
export function dockIntent(value:DockDocument,intent:DockIntent):DockDocument {
  if(!plain(intent)||intent.kind!=='dock')fail()
  let {counter}=value
  // Prefixes are layout identities only; no runtime authority is persisted.
  const next=((prefix:string)=>`${prefix}${++counter}`) as plan.Mint
  const s=value.state,t=intent.tab as TabId,p=intent.pane as PaneId
  let ops:readonly LayoutOp[]=[], result
  switch(intent.operation){
    case 'open': {
      const c=coordinate(intent.coordinate),isGuide=c.action==='resources'
      const held=isGuide?guideInPane(s,p??plan.activeDockPaneId(s)):undefined
      ops=held?focusTab(s,held):plan.planOpenContent(s,next,{
        kind:'rsi',contentId:JSON.stringify(c),title:text(intent.title,512)?intent.title:'Resource',
        revealIfOpened:!isGuide&&intent.duplicate!==true,paneId:p,
      }).ops
      if(!s.expanded)ops=[...plan.planSetExpanded(s,true),...ops]
      break
    }
    case 'close':ops=[{type:'closeTab',tabId:t}];break
    case 'focus':ops=focusTab(s,t);break
    case 'focus-pane':
      if(p===s.activePaneId)return value
      ops=[{type:'focusPane',paneId:p}];break
    case 'split':
      if(dockPaneIds(s).length<2){
        const guide=Object.keys(s.tabs).length<16
          ? (id:TabId)=>({id,kind:'rsi',contentId:guideContentId,title:'Resources'})
          : undefined
        ops=plan.planSplitPane(s,next,p,guide)
      }
      break
    case 'duplicate':if(s.tabs[t]?.contentId!==guideContentId)ops=plan.planDuplicateTab(s,next,t).ops;break
    case 'place':ops=mergeArrivingGuide(s,t,p)??plan.planPlaceTab(s,t,p,intent.index as number);break
    case 'drop':
      if(intent.zone==='center')ops=mergeArrivingGuide(s,t,p)??plan.planDropTab(s,next,t,p,'center')
      else if((intent.zone==='left'||intent.zone==='right')&&dockPaneIds(s).length<2)ops=plan.planDropTab(s,next,t,p,intent.zone)
      break
    case 'float':
      if(s.floats.length<4||findTabPane(s,t)?.host==='float')
        ops=plan.planFloatTab(s,next,t,intent.rect===undefined?undefined:{...checkedFloatRect(intent.rect)}).ops
      break
    case 'dock':{
      const source=getPane(s,p)
      ops=(source.host==='float'?mergeArrivingGuide(s,source.tabs[0]!,plan.activeDockPaneId(s)):undefined)??plan.planUnfloatPane(s,p)
      break
    }
    case 'move-float':
      if(!floatPixel(intent.x)||!floatPixel(intent.y))return fail()
      ops=[{type:'moveFloat',paneId:p,x:intent.x,y:intent.y}];break
    case 'resize-float':ops=[{type:'resizeFloat',paneId:p,rect:{...checkedFloatRect(intent.rect)}}];break
    case 'resize':ops=plan.planResizeSplit(intent.split as SplitId,intent.sizes as number[],.2);break
    case 'expanded':if(typeof intent.expanded!=='boolean')return fail();ops=plan.planSetExpanded(s,intent.expanded);break
    case 'toggle-fullscreen':
      ops=intent.automatic===true
        ? [...plan.planSetExpanded(s,false),...plan.planSetMode(s,'push')]
        : plan.planSetMode(s,s.mode==='push'?'fullscreen':'push')
      break
    case 'undo':result=stepBack(value.history,s);break
    case 'redo':result=stepForward(value.history,s);break
    default:fail()
  }
  if(!result && ops.length===0)return value
  if(['close','place','drop','float','dock'].includes(intent.operation)&&ops.length){
    const after=ops.reduce((state,op)=>applyOp(state,op).state,s)
    ops=[...ops,...plan.planSettle(after,next)]
  }
  if(counter>1e14)fail()
  result??=record(value.history,s,ops)
  if(result.state.tabs!==s.tabs && Object.keys(result.state.tabs).length>16)fail()
  const history=result.history.entries.length>64?{entries:result.history.entries.slice(-64),cursor:64}:result.history
  return {...value,counter,state:result.state,history}
}
export function floatingTab(state:LayoutState,tab:TabId) {return findTabPane(state,tab).host==='float'}
export {dockPaneIds,findTabPane,getPane}

// Display-only geometry keeps the desktop layout intact on narrow screens.
export function narrowDock(state:LayoutState):LayoutState {
  const ids=dockPaneIds(state),root=getPane(state,ids[0]!)
  if(ids.length===1)return state
  const tabs=ids.flatMap(id=>getPane(state,id).tabs)
  const active=getPane(state,state.activePaneId)
  const selected=active.host==='float'?root.activeTabId:active.activeTabId
  const pane={...root,tabs,activeTabId:tabs.includes(selected!)?selected:tabs[0]}
  const nodes=Object.fromEntries(state.floats.map(id=>[id,state.nodes[id]!]))
  nodes[root.id]=pane
  return {...state,nodes,rootId:root.id,activePaneId:active.host==='float'?active.id:root.id}
}

export function displayDock(state:LayoutState,width:number,height:number,narrow:boolean):LayoutState {
  const shown=narrow?narrowDock(state):state
  if(!shown.floats.length)return shown
  const nodes={...shown.nodes}
  for(const id of shown.floats){
    const pane=getPane(shown,id),rect=pane.rect!
    const w=Math.min(rect.width,Math.max(100,width-16)),h=Math.min(rect.height,Math.max(100,height-16))
    nodes[id]={...pane,rect:{x:Math.max(8,Math.min(rect.x,width-w-8)),y:Math.max(8,Math.min(rect.y,height-h-8)),width:w,height:h}}
  }
  return {...shown,nodes}
}
