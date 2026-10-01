import React from 'react'
import {createRoot} from 'react-dom/client'
import {DockLayout} from '@fixture/dock/components/DockSurface.tsx'
import {createInitialState,createIdMinter} from '@fixture/dock/engine/initial.ts'
import {record,EMPTY_HISTORY} from '@fixture/dock/engine/sequence.ts'
import * as plan from '@fixture/dock/engine/planner.ts'
import {dockPaneIds} from '@fixture/dock/engine/tree.ts'
import '@fixture/theme'
import './scene.css'
const dark=new URLSearchParams(location.search).get('theme')==='dark'
document.documentElement.dataset.theme=dark?'dark':'light'
if(dark)document.body.setAttribute('data-ds-dark-theme','')
const minter=createIdMinter(),mint=minter.next;let state={...createInitialState(minter),expanded:true}
const commit=ops=>{state=record(EMPTY_HISTORY,state,ops).state}
const first=plan.planOpenContent(state,mint,{kind:'fixture',contentId:'one',title:'Workspace files'});commit(first.ops)
commit(plan.planOpenContent(state,mint,{kind:'fixture',contentId:'two',title:'Source preview'}).ops)
commit(plan.planSplitPane(state,mint,state.activePaneId))
commit(plan.planOpenContent(state,mint,{kind:'fixture',contentId:'three',title:'Terminal',paneId:dockPaneIds(state)[1]}).ops)
const floating=plan.planOpenContent(state,mint,{kind:'fixture',contentId:'four',title:'Changes'});commit(floating.ops)
commit(plan.planFloatTab(state,mint,floating.tabId,{x:120,y:360,width:420,height:240}).ops)
const labels={emptyPane:'Open a resource',splitPane:'Split resources',splitPaneDisabled:'Two panes',splitPaneNarrow:'Widen resources',closeTab:'Close resource tab',addTab:'Open resources',dockFloat:'Dock resource',closeFloat:'Close resource window',dropZone:{center:'Merge tabs',left:'Split left',right:'Split right',top:'',bottom:''}}
const noop=()=>{},intents=Object.fromEntries(['focusTab','focusPane','splitPane','addTab','closeTab','duplicateTab','floatTab','unfloatPane','placeTab','dropTab','moveFloat','resizeFloat','resizeSplit'].map(name=>[name,noop]))
createRoot(document.getElementById('root')).render(<section className="resource-dock"><DockLayout state={state} active keepMounted={()=>true} canSplit={false} dropZones="horizontal" minPaneFraction={.2} labels={labels} intents={intents} renderTab={tab=><div className="fixture-content"><h2>{tab.title}</h2><p>One independent resource view.</p><pre>ready: true{'\n'}generation: 1</pre></div>}/></section>);
window.fixtureReady=true
