import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {sessionHelper} from '../../../crates/rsi/browser/runtime/session-helper.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const source=`
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {flushSync} from 'react-dom';
import {DockLayout} from '/vendor/dsh/dockkit/components/DockSurface.tsx';
import {createInitialState} from '/vendor/dsh/dockkit/engine/initial.ts';
import {record,stepBack,EMPTY_HISTORY} from '/vendor/dsh/dockkit/engine/sequence.ts';
import {planOpenContent,planSplitPane,planFloatTab} from '/vendor/dsh/dockkit/engine/planner.ts';
import css from '/src/resources.module.css';
import '/styles.css';
let n=0;const mint=prefix=>prefix+(++n);
let seed={...createInitialState({next:mint}),expanded:true};
for(let i=0;i<3;i++)seed=record(EMPTY_HISTORY,seed,planOpenContent(seed,mint,{kind:'fixture',contentId:'file'+i,title:'File '+i}).ops).state;
seed=record(EMPTY_HISTORY,seed,planSplitPane(seed,mint)).state;
const first=Object.keys(seed.tabs)[0];
seed=record(EMPTY_HISTORY,seed,planFloatTab(seed,mint,first,{x:100,y:100,width:300,height:220}).ops).state;
const labels={emptyPane:'Open a resource',splitPane:'Split',splitPaneDisabled:'Two panes',splitPaneNarrow:'Widen',closeTab:'Close tab',addTab:'Add',dockFloat:'Dock',closeFloat:'Close window',resizePane:'Resize panes',moveFloat:'Move window',resizeFloat:'Resize window',describeFloatPosition:(x,y)=>'Position '+x+', '+y,describeFloatSize:(w,h)=>'Size '+w+' by '+h,dropZone:{center:'Merge',left:'Left',right:'Right',top:'Top',bottom:'Bottom'}};
function App(){
 const [current,set]=useState({state:seed,history:EMPTY_HISTORY});
 const apply=op=>set(c=>record(c.history,c.state,[op]));
 window.dockState=()=>current.state;
 window.undo=()=>flushSync(()=>set(c=>stepBack(c.history,c.state)??c));
 window.setFloat=rect=>flushSync(()=>apply({type:'resizeFloat',paneId:current.state.floats[0],rect}));
 const noop=()=>{};
 const intents={...Object.fromEntries(['focusTab','splitPane','addTab','closeTab','duplicateTab','floatTab','unfloatPane','placeTab','dropTab'].map(k=>[k,noop])),focusPane:paneId=>apply({type:'focusPane',paneId}),moveFloat:(paneId,x,y)=>apply({type:'moveFloat',paneId,x,y}),resizeFloat:(paneId,rect)=>apply({type:'resizeFloat',paneId,rect}),resizeSplit:(splitId,sizes)=>apply({type:'resize',splitId,sizes})};
 return <section className={css.scope} style={{height:500,width:900,display:'flex',flexDirection:'column'}}><DockLayout state={current.state} active minPaneFraction={.2} labels={labels} intents={intents} renderTab={tab=><p>{tab.title}</p>}/></section>;
}
createRoot(document.getElementById('root')).render(<App/>);
`;
const fixture=await sourceFixture({name:'dock-keyboard',source}),results=[];
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch();
  try {
   const page=await browser.newPage({viewport:{width:1000,height:800}}),errors=[];
   page.on('pageerror',error=>{errors.push(error.message);console.error(error.message)});
   await page.goto(fixture.url);
   const divider=page.getByRole('separator',{name:'Resize panes'});
   await divider.waitFor();await divider.focus();
   await page.keyboard.press('ArrowRight');assert.equal(await divider.getAttribute('aria-valuenow'),'52');
   assert(await divider.evaluate(el=>el===document.activeElement));
   await page.evaluate(()=>window.undo());assert.equal(await divider.getAttribute('aria-valuenow'),'50');
   await page.keyboard.press('Home');assert.equal(await divider.getAttribute('aria-valuenow'),'20');
   await page.keyboard.press('End');assert.equal(await divider.getAttribute('aria-valuenow'),'80');
   await page.keyboard.press('Shift+ArrowLeft');assert.equal(await divider.getAttribute('aria-valuenow'),'70');
   const move=page.getByRole('button',{name:'Move window: File 0'}),resize=page.getByRole('button',{name:'Resize window',exact:true});
   await move.focus();await page.keyboard.press('ArrowRight');assert.match(await move.getAttribute('aria-description'),/110, 100/);
   await page.keyboard.press('Shift+ArrowDown');assert.match(await move.getAttribute('aria-description'),/110, 150/);
   assert(await move.evaluate(el=>el===document.activeElement));
   await page.evaluate(()=>window.undo());assert.match(await move.getAttribute('aria-description'),/110, 100/);
   await page.keyboard.press('Home');assert.match(await move.getAttribute('aria-description'),/0, 0/);
   await page.keyboard.press('End');assert.match(await move.getAttribute('aria-description'),/700, 580/);
   await page.keyboard.press('Home');
   await resize.focus();await page.keyboard.press('Shift+ArrowRight');assert.match(await resize.getAttribute('aria-description'),/350 by 220/);
   await page.keyboard.press('ArrowDown');assert.match(await resize.getAttribute('aria-description'),/350 by 230/);
   await page.keyboard.press('Home');assert.match(await resize.getAttribute('aria-description'),/220 by 140/);
   await page.keyboard.press('End');assert.match(await resize.getAttribute('aria-description'),/1000 by 800/);
   assert(await resize.evaluate(el=>el===document.activeElement));
   const edge={x:980,y:780,width:300,height:220};
   await page.evaluate(rect=>window.setFloat(rect),edge);
   await page.waitForFunction(()=>document.querySelector('[data-dockkit-float-title]').getAttribute('aria-description')==='Position 980, 780');
   await resize.focus();await page.keyboard.press('End');
   const edgeSize=await resize.getAttribute('aria-description');
   await writeFile(join(report,name+'-edge.json'),JSON.stringify({edge,edgeSize}));
   assert.match(edgeSize,/220 by 140/,'viewport clipping must not shrink below the float minimum');
   await page.evaluate(()=>window.setFloat({x:0,y:0,width:220,height:140}));
   await page.waitForFunction(()=>document.querySelector('[data-dockkit-float-title]').getAttribute('aria-description')==='Position 0, 0');
   await page.keyboard.press('Home');await move.focus();await page.keyboard.press('Shift+ArrowRight');await page.keyboard.press('Shift+ArrowDown');
   await page.screenshot({path:join(report,name+'-keyboard.png'),fullPage:true});
   assert.deepEqual(errors,[]);
   // The production helper observes a real DOM with a hidden prefix beyond its cap.
   await page.setContent('<main>'+('<button hidden>Hidden</button>'.repeat(300))+'<button id="visible">Visible action</button></main>');
   const helper=await sessionHelper(browser,{mode:'local_dev',origin:new URL(fixture.url).origin});
   const observation=await helper({operation:'observe'});
   assert.equal(observation.snapshot.nodes.length,1);assert.equal(observation.snapshot.nodes[0].name,'Visible action');assert.equal(observation.snapshot.truncated,false);
   await page.locator('#visible').evaluate(el=>el.hidden=true);
   const refused=await helper({operation:'click',document_version:observation.snapshot.document_version,observation_id:observation.snapshot.observation_id,node:'1'});
   assert.equal(refused.status,'not_started');
   results.push({browser:name,version:browser.version(),keyboard:true,undo:true,focus:true,viewportBounds:true,hiddenPrefix:300,staleActionRefused:true,errors});
  } finally {await browser.close()}
 }
} finally {await fixture.close();await writeFile(join(report,'result.json'),JSON.stringify(results,null,2))}
