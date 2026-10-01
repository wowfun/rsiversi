// RSI policy around the pinned, host-independent DSH operation engine.
import {createInitialState,createIdMinter} from '../vendor/dsh/dockkit/engine/initial.ts'
import {applyOp} from '../vendor/dsh/dockkit/engine/operations.ts'
import * as plan from '../vendor/dsh/dockkit/engine/planner.ts'
import {record,stepBack,stepForward,EMPTY_HISTORY,type History} from '../vendor/dsh/dockkit/engine/sequence.ts'
import {dockPaneIds,findTabPane,getPane} from '../vendor/dsh/dockkit/engine/tree.ts'
import type {LayoutState,LayoutOp,TabId,PaneId,SplitId,FloatRect,DockZone} from '../vendor/dsh/dockkit/contract/types.ts'

export type Coordinate = {action:string;[key:string]:unknown}
export interface DockDocument {kind:'session-dock';version:1;counter:number;state:LayoutState;history:History}
export type DockIntent = {kind:'dock';operation:string;[key:string]:unknown}
const plain=(v:unknown):v is Record<string,any>=>!!v&&typeof v==='object'&&!Array.isArray(v)
const keys=(v:unknown,list:string)=>plain(v)&&Object.keys(v).sort().join()===list.split(',').sort().join()
const text=(v:unknown,max=256):v is string=>typeof v==='string'&&v.length>0&&new TextEncoder().encode(v).length<=max&&!/[\x00-\x1f\x7f]/.test(v)
const id=(v:unknown):v is string=>text(v,80)&&/^(pane|split|tab|float)[1-9][0-9]{0,14}$/.test(v)
const fail=():never=>{throw new Error('Invalid resource layout')}
export function coordinate(value:unknown):Coordinate {
  if(!plain(value)||!text(value.action,40)||JSON.stringify(value).length>16384)fail()
  const v=value as Record<string,any>
  const valid=v.action==='terminal' ? keys(v,'action,terminal')&&text(v.terminal) :
    v.action==='resources' ? keys(v,'action') :
    ['surface','application_surface','remote_surface'].includes(v.action) ? keys(v,'action,bundle,name')&&text(v.bundle,128)&&text(v.name,128) :
    ['ui_block','inspect_block'].includes(v.action) ? keys(v,'action,key')&&text(v.key,512) :
    v.action==='inspect_image' ? keys(v,'action,media')&&plain(v.media) :
    v.action==='inspect_source' ? keys(v,'action,source')&&plain(v.source) :
    v.action==='inspect_interaction' ? keys(v,'action,owner,id')&&text(v.owner)&&text(v.id) :
    v.action==='remote_ui_list' ? keys(v,'action') : false
  if(!valid)fail()
  // MediaRef and source coordinates receive their authoritative typed checks in
  // Rust before any read. Never retain a presentation/action/terminal attachment.
  return structuredClone(v) as Coordinate
}
export function emptyDock():DockDocument {
  const minter=createIdMinter()
  return {kind:'session-dock',version:1,counter:1,state:{...createInitialState(minter),expanded:true},history:EMPTY_HISTORY}
}
export function validateDock(value:unknown):DockDocument {
  if(!keys(value,'kind,version,counter,state,history'))fail()
  const doc=value as DockDocument
  if(doc.kind!=='session-dock'||doc.version!==1||!Number.isSafeInteger(doc.counter)||doc.counter<1||doc.counter>1e14)fail()
  validateState(doc.state,doc.counter)
  if(!keys(doc.history,'entries,cursor')||!Array.isArray(doc.history.entries)||doc.history.entries.length>64||!Number.isInteger(doc.history.cursor)||doc.history.cursor<0||doc.history.cursor>doc.history.entries.length)fail()
  // Stored inverses are not trusted executable commands. Verify every reachable
  // state through both directions before allowing this history to be replayed.
  for(const entry of doc.history.entries)if(!keys(entry,'ops,inverse')||!Array.isArray(entry.ops)||!Array.isArray(entry.inverse)||entry.ops.length>16||entry.inverse.length>32)fail()
  try {
    let step={state:doc.state,history:doc.history}
    while(step.history.cursor>0){step=stepBack(step.history,step.state) ?? fail();validateState(step.state,doc.counter)}
    while(step.history.cursor<step.history.entries.length){step=stepForward(step.history,step.state) ?? fail();validateState(step.state,doc.counter)}
  }catch{fail()}
  return doc
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
      if(floating){const r=node.rect;if(!r||!keys(r,'x,y,width,height')||Object.values(r).some(n=>!Number.isFinite(n)||Math.abs(n)>100000)||r.width<100||r.height<100)fail()}else if(node.rect!==undefined)fail()
      if(node.tabs.length?!node.tabs.includes(node.activeTabId!):node.activeTabId!==undefined)fail()
      for(const tabId of node.tabs){if(!id(tabId)||Number(tabId.replace(/^[a-z]+/,''))>counter||tabs.has(tabId))fail();tabs.add(tabId);const tab=state.tabs[tabId];if(!keys(tab,'id,kind,contentId,title')||tab.id!==tabId||tab.kind!=='rsi'||!text(tab.title,512))fail();coordinate(JSON.parse(tab.contentId))}
    }else fail()
  }
  visit(state.rootId,false);state.floats.forEach(float=>visit(float,true))
  if(docked>2||seen.size!==Object.keys(state.nodes).length||tabs.size!==Object.keys(state.tabs).length||!seen.has(state.activePaneId)||state.nodes[state.activePaneId]?.kind!=='pane')fail()
}
export function dockIntent(value:DockDocument,intent:DockIntent):DockDocument {
  validateDock(value)
  if(!plain(intent)||intent.kind!=='dock')fail()
  let {counter}=value
  // Prefixes are layout identities only; no runtime authority is persisted.
  const next=((prefix:string)=>`${prefix}${++counter}`) as plan.Mint
  const s=value.state,t=intent.tab as TabId,p=intent.pane as PaneId
  let ops:readonly LayoutOp[]=[], result
  switch(intent.operation){
    case 'open': {const c=coordinate(intent.coordinate);ops=plan.planOpenContent(s,next,{kind:'rsi',contentId:JSON.stringify(c),title:text(intent.title,512)?intent.title:'Resource',revealIfOpened:intent.duplicate!==true,...(p?{paneId:p}:{})}).ops;break}
    case 'close':ops=[{type:'closeTab',tabId:t}];break
    case 'focus':ops=[{type:'focusTab',tabId:t}];break
    case 'focus-pane':ops=[{type:'focusPane',paneId:p}];break
    case 'split':if(dockPaneIds(s).length<2)ops=plan.planSplitPane(s,next,p);break
    case 'duplicate':ops=plan.planDuplicateTab(s,next,t).ops;break
    case 'place':ops=plan.planPlaceTab(s,t,p,intent.index as number);break
    case 'drop':if(['center','left','right'].includes(intent.zone as string)&&(intent.zone==='center'||dockPaneIds(s).length<2))ops=plan.planDropTab(s,next,t,p,intent.zone as DockZone);break
    case 'float':if(s.floats.length<4||findTabPane(s,t)?.host==='float')ops=plan.planFloatTab(s,next,t,intent.rect as FloatRect|undefined).ops;break
    case 'dock':ops=plan.planUnfloatPane(s,p);break
    case 'move-float':ops=[{type:'moveFloat',paneId:p,x:intent.x as number,y:intent.y as number}];break
    case 'resize-float':ops=[{type:'resizeFloat',paneId:p,rect:intent.rect as FloatRect}];break
    case 'resize':ops=plan.planResizeSplit(intent.split as SplitId,intent.sizes as number[],.2);break
    case 'mode':if(intent.mode!=='push'&&intent.mode!=='fullscreen')fail();ops=plan.planSetMode(s,intent.mode as 'push'|'fullscreen');break
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
  // Check the two-pane/float/tab constraints after every semantic operation.
  validateState(result.state,counter)
  let history=result.history.entries.length>64?{entries:result.history.entries.slice(-64),cursor:64}:result.history
  // Checkpoint older undo steps before the durable record budget is reached.
  // Open coordinates are never evicted to make room for history.
  while(history.cursor>0 && new TextEncoder().encode(JSON.stringify({state:result.state,history})).length>28*1024)
    history={entries:history.entries.slice(1),cursor:history.cursor-1}
  return {...value,counter,state:result.state,history}
}
export function floatingTab(state:LayoutState,tab:TabId) {return findTabPane(state,tab)?.host==='float'}
export function paneFor(state:LayoutState,tab:TabId){return findTabPane(state,tab)?.id}
export {dockPaneIds,getPane}

// A display projection only: focus still targets the tab in the committed tree.
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
  const shown=narrow?narrowDock(state):state,nodes={...shown.nodes}
  for(const id of shown.floats){
    const pane=getPane(shown,id),rect=pane.rect!
    const w=Math.min(rect.width,Math.max(100,width-16)),h=Math.min(rect.height,Math.max(100,height-16))
    nodes[id]={...pane,rect:{x:Math.max(8,Math.min(rect.x,width-w-8)),y:Math.max(8,Math.min(rect.y,height-h-8)),width:w,height:h}}
  }
  return {...shown,nodes}
}
