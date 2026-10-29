import assert from 'node:assert/strict';
import {join} from 'node:path';
import {sourceFixture} from './source-fixture.mjs';

// Actual React controls and the production draft owner; no ACP transport claim.
export async function verifyExternalDraftDom(browser, root, report, name) {
 const server=await sourceFixture({name:'external-fixture',source:`
import React,{useSyncExternalStore} from 'react';
import {createRoot} from 'react-dom/client';
import {ExternalSurface} from './src/session/external.tsx';
import {externalPaneClass} from './src/runtime/external-controller.js';
import {observable} from './src/bridge.ts';
import './styles.css';
const owners=observable([]);let fail=true,calls=0;
const Pane=externalPaneClass({command:async()=>{calls++;if(fail)throw Error('unknown')},perform:work=>Promise.resolve().then(work).catch(()=>{})});
const frame=(id,generation=id)=>({generation,external:{observed:{connected:true,snapshot:{id,generation:'1',endpoint:'Fixture agent',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:[],following:true,busy:false}});
const stage=(pane,data)=>{pane.staged={data,submitKey:'enter'};pane.render(data)};
const add=(id)=>{const pane=new Pane(String(owners.getSnapshot().length));stage(pane,frame(id));owners.set([...owners.getSnapshot(),pane]);return pane};
function Scene(){const panes=useSyncExternalStore(owners.subscribe,owners.getSnapshot);return <main id="panes" className="panes" style={{display:'flex',height:'100vh'}}>{panes.map(pane=><ExternalSurface key={pane.index} owner={pane} selected={Number(pane.index)<2}/>)}</main>}
createRoot(document.getElementById('root')).render(<Scene/>);add('A');add('A');
window.externalFixture={Pane,frame,stage,add,get panes(){return owners.getSnapshot()},calls:()=>calls,succeed:()=>{fail=false},reset(){for(const pane of owners.getSnapshot())pane.reset();owners.set([]);Pane.clearDrafts()}};
`});
 const page=await browser.newPage({viewport:{width:1440,height:980}}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 try {
  await page.goto(server.url);
  const panes=page.locator('.external-pane'),input=panes.first().getByLabel('External message');
  await input.fill('Retain this prompt · 保留原始草稿');
  assert.equal(await panes.nth(1).getByLabel('External message').inputValue(),await input.inputValue());
  await panes.first().getByTestId('composer-send').click();
  await panes.first().getByRole('alert').filter({hasText:'outcome is unknown'}).waitFor();
  await page.evaluate(()=>{const f=externalFixture;f.stage(f.panes[0],f.frame('B'));f.stage(f.panes[0],f.frame('A','reattached'))});
  assert.equal(await input.inputValue(),'Retain this prompt · 保留原始草稿');
  assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),true);
  assert.equal(await panes.nth(1).getByTestId('composer-send').isDisabled(),true);
  assert.equal(await page.evaluate(()=>externalFixture.calls()),1);
  const mutations=await page.evaluate(async()=>{
   const pane=externalFixture.panes[0],records=[];
   const observer=new MutationObserver(next=>records.push(...next));observer.observe(pane.node.querySelector('[role="alert"]'),{subtree:true,childList:true,characterData:true,attributes:true});
   for(let i=0;i<3;i++){pane.renderComposer();pane.input.dispatchEvent(new Event('input',{bubbles:true}))}
   await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));observer.disconnect();return records.length;
  });
  assert.equal(mutations,0,'unchanged warning must not retrigger its live region');
  await page.screenshot({path:join(report,name+'-external-unknown.png')});
  await input.fill('Explicitly revised');assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),false);
  await page.evaluate(()=>{const f=externalFixture;for(let i=0;i<15;i++)f.add('pinned-'+i);const state=f.frame('overflow');state.external.blocks=[{key:'history',role:'agent',text:'Retained observation while draft capacity is full',source:{}}];f.blocked=state;f.stage(f.panes[1],state)});
  assert.equal(await panes.nth(1).getByLabel('External message').isDisabled(),true);
  await page.evaluate(()=>{window.historyRow=externalFixture.panes[1].node.querySelector('.external-record');externalFixture.stage(externalFixture.panes[1],externalFixture.blocked)});
  assert.equal(await page.evaluate(()=>historyRow===externalFixture.panes[1].node.querySelector('.external-record')),true);
  await page.evaluate(()=>{externalFixture.panes[2].reset();externalFixture.stage(externalFixture.panes[1],externalFixture.blocked)});
  assert.equal(await panes.nth(1).getByLabel('External message').isDisabled(),false);
  await panes.nth(1).getByLabel('External message').fill('Capacity released');
  await page.evaluate(()=>{externalFixture.reset();for(let i=0;i<16;i++)externalFixture.add('full-'+i);externalFixture.succeed()});
  await page.waitForFunction(()=>externalFixture.panes.length===16&&externalFixture.panes.every(p=>p.input));
  await page.evaluate(()=>{for(const pane of externalFixture.panes){pane.input.value='x'.repeat(65536);if(!pane.save())throw Error('fixture admission failed')}});
  await input.press('Control+End');await input.press('!');
  assert.equal((await input.inputValue()).length,65536);assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),false);
  await page.evaluate(()=>externalFixture.add('full-0'));await page.waitForFunction(()=>externalFixture.panes.at(-1).input);
  await page.evaluate(()=>{const pane=externalFixture.panes.at(-1);pane.input.value='Short accepted revision';pane.save()});
  assert.equal(await input.inputValue(),'Short accepted revision');assert.equal(await panes.first().getByRole('alert').isVisible(),false);
  for(const theme of ['light','dark']){await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await page.screenshot({path:join(report,name+'-external-recovery-'+theme+'.png')})}
  await input.fill('x'.repeat(65536));await input.press('Control+End');await input.press('!');
  await page.setViewportSize({width:420,height:860});await page.screenshot({path:join(report,name+'-external-rejected-narrow.png')});
  await panes.first().getByTestId('composer-send').click();await page.waitForFunction(()=>externalFixture.panes[0].input.value==='');
  await input.fill('x'.repeat(65536));await input.press('Control+End');await input.press('!');await input.press('Enter');
  await page.waitForFunction(()=>externalFixture.panes[0].input.value==='');assert.equal(await page.evaluate(()=>externalFixture.calls()),3);
  assert.deepEqual(errors,[]);
 } finally {await page.close();await server.close()}
}
