import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const source=`
import React,{useState,useSyncExternalStore} from 'react';
import {createRoot} from 'react-dom/client';
import {flushSync} from 'react-dom';
import {createResourceDetails} from '/src/runtime/resource-details.js';
import {Transcript} from '/src/session/transcript.tsx';
import {SessionCommands} from '/src/session/projections.tsx';
import {DraftImages,DraftReferences} from '/src/session/draft-attachments.tsx';
import {ResourcePresentations} from '/src/details/resources.tsx';
import {resourceHosts,resources,publishResources} from '/src/resource-hosts.ts';
import {notices,observable} from '/src/bridge.ts';
import {usePresentation,presentationIdentity,ResizeHandle} from '/src/presentation.tsx';
import {PresentationStore} from '/presentation-store.js';
import {installDisclosures} from '/disclosure.js';
import {Button} from '/src/button.tsx';
import {keyedObservableHook} from '/vendor/dsh/renderer/bindings.tsx';
import {DockLayout} from '/vendor/dsh/dockkit/components/DockSurface.tsx';
import {createInitialState} from '/vendor/dsh/dockkit/engine/initial.ts';
import {record,EMPTY_HISTORY} from '/vendor/dsh/dockkit/engine/sequence.ts';
import {planOpenContent} from '/vendor/dsh/dockkit/engine/planner.ts';
import session from '/src/session/surface.module.css';
import resourceCss from '/src/resources.module.css';
import '/styles.css';
window.actions=[];
// Observe only the real component's offset Maps; no production test port is added.
const mapSet=Map.prototype.set,offsetMaps=new Set();
Map.prototype.set=function(key,value){if(typeof key==='string'&&key.startsWith('offset-probe-pane')&&typeof value==='number')offsetMaps.add(this);return mapSet.call(this,key,value)};
window.offsetEntries=()=>[...offsetMaps].flatMap(map=>[...map.entries()]);
let nextDockId=0;
function dockState(){
 const mint=prefix=>'offset-probe-'+prefix+(++nextDockId);
 let state={...createInitialState({next:mint}),expanded:true};
 for(let i=0;i<10;i++)state=record(EMPTY_HISTORY,state,planOpenContent(state,mint,{kind:'fixture',contentId:'file-'+i,title:'Files '+i}).ops).state;
 return state;
}
const dockLabels={emptyPane:'Open a resource',splitPane:'Split resources',splitPaneDisabled:'Two panes',splitPaneNarrow:'Widen resources',closeTab:'Close resource tab',addTab:'Open resources',dockFloat:'Dock resource',closeFloat:'Close resource window',dropZone:{center:'Merge tabs',left:'Split left',right:'Split right',top:'',bottom:''}};
const noop=()=>{};
function DockProbe(){
 const [state,setState]=useState(dockState);
 window.replaceDock=()=>flushSync(()=>setState(dockState()));
 window.dockPane=()=>state.rootId;
 const intents={...Object.fromEntries(['focusPane','splitPane','addTab','closeTab','duplicateTab','floatTab','unfloatPane','placeTab','dropTab','moveFloat','resizeFloat','resizeSplit'].map(name=>[name,noop])),focusTab:id=>setState(current=>record(EMPTY_HISTORY,current,[{type:'focusTab',tabId:id}]).state)};
 return <section aria-label="Dock lifecycle" className={resourceCss.scope} style={{width:480,height:180}}><DockLayout state={state} active canSplit={false} labels={dockLabels} intents={intents} renderTab={tab=><p>{tab.title}</p>}/></section>;
}
const change=observable(0);
const member=observable(1),useMember=keyedObservableHook((key,name)=>key==='session'&&name==='usage'?member:undefined);
window.updateMember=()=>member.set(2);
const owner={blocks:new Map(),commandsExpanded:true,readingSummary:false,setReadingSummary(value){this.readingSummary=value;change.set(change.getSnapshot()+1)},action:async(action)=>window.actions.push(action),selectCommand(){throw new Error('Draft is loading')},moveImage(){throw new Error('Image draft is loading')},previewReference(){throw new Error('Reference draft is loading')}};
const transcript={blocks:[{key:'tool',role:'tool',title:'Delegate',text:'delegated',external:{id:'external'},sources:1}],turns:{entries:{turn:{id:'turn',status:'Completed',running:false,foldable:true,partial:false,blocks:['tool'],process:['tool'],candidate:[],answer:[]}}}};
const body=document.createElement('section');body.className=resourceCss.scope;document.body.append(body);
const entry={view:'sources',reopen:{large:'x'.repeat(262144)}};
resourceHosts.set('sources',{body,entry,title:'Sources',key:'sources',model:{kind:'block-sources',command:async(value)=>window.actions.push(value.action),data:{start:0,total:1,page:[{seq:'7',field:{kind:'text'}}]}}});publishResources();
window.resourceIdentity=()=>{const before=resources.getSnapshot();entry.reopen.toJSON=()=>{throw new Error('Panel metadata serialized')};publishResources();delete entry.reopen.toJSON;return before===resources.getSnapshot()};
window.updateResource=(seq,key)=>{const host=resourceHosts.get('sources');host.model={...host.model,data:{...host.model.data,page:[{seq,field:{kind:'text'}}]}};if(key)host.key=key;publishResources()};
window.imageReads=0;window.imageReleases=0;
const imageBody=document.createElement('section');document.body.append(imageBody);
const imageContext={connection:{},boundCall:()=>async()=>{},acquireImage(){window.imageReads++;const url=URL.createObjectURL(new Blob(['<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><rect width="24" height="24" fill="blue"/></svg>'],{type:'image/svg+xml'}));return {result:Promise.resolve(url),release(){window.imageReleases++;URL.revokeObjectURL(url)}}}};
const imageRender=createResourceDetails(imageContext);
const imageHost={body:imageBody,entry:{view:'image'}};
window.imageFrame=(ticket='1',reconnect=false)=>{if(reconnect)imageContext.connection={};flushSync(()=>imageRender({image_detail:{ticket,media:{width:24,height:24,bytes:100}}},{show(key,title,model){Object.assign(imageHost,{key,title,model});resourceHosts.set('image',imageHost);publishResources()}}))};
window.closeImage=()=>flushSync(()=>{resourceHosts.delete('image');publishResources()});
const original=PresentationStore.open;let fail=true;
PresentationStore.open=async(...args)=>{if(fail)throw new Error('blocked');return original(...args)};
presentationIdentity.set('review-layout');
function App(){
 const memberValue=useMember('session','usage');
 useSyncExternalStore(change.subscribe,change.getSnapshot);
 const notice=useSyncExternalStore(notices.subscribe,notices.getSnapshot),layout=usePresentation();
 window.layout=layout;window.enableStorage=()=>{fail=false;layout.retry()};
 return <main><output aria-label="Member value">{memberValue}</output><section className={session.scope+' mode-standard'}><Transcript owner={owner} transcript={transcript} generation="1" mode="standard" cards={false} register={()=>{}}/><button hidden={!owner.readingSummary} onClick={()=>owner.setReadingSummary(false)}>Back to bottom</button></section>
 <SessionCommands data={{commands:{revision:{kind:'draft'},commands:[{name:'help',description:'Help',draft_safe:true}]}}} owner={owner}/>
 <DraftImages owner={owner} state={{images:[{width:1,height:1,bytes:4}]}}/>
 <DraftReferences owner={owner} state={{references:[{id:'r',label:'Reference'}]}}/>
 <output aria-label="Notice">{notice.transient}</output><output aria-label="Layout storage">{layout.storageNotice}</output><output aria-label="Width">{layout.layout.navigationWidth}</output>
 <div id="workbench" style={{marginLeft:80}}><aside style={{width:"var(--resources-width,450px)",height:40}}><ResizeHandle name="resources" value={800} min={300} max={700} reverse onChange={value=>window.resized=value}/></aside></div><style>{"#workbench .panel-resize{height:40px;width:6px;background:gray}"}</style><ResourcePresentations/><details className="application-options"><summary>Application options</summary><Button onClick={()=>window.actions.push('menu')}>Menu action</Button></details><button>Outside</button>
 <section aria-label="Status colors">{['warning','success','teal'].map(color=><p key={color} style={{color:'var(--'+color+')',background:color==='warning'?'var(--warning-bg)':color==='teal'?'var(--blue)':'var(--paper)'}}>{color} status and link</p>)}</section><DockProbe/></main>
}
installDisclosures(document);createRoot(document.getElementById('root')).render(<App/>);
`;
const fixture=await sourceFixture({name:'review-regressions',source}),results=[];
try {
  for(const [name,engine] of Object.entries({chromium,firefox})) {
    const browser=await engine.launch();let page;
    try {
      page=await browser.newPage({viewport:{width:1000,height:900}});
      const errors=[];page.on('pageerror',error=>{errors.push(error.message);console.error(error.message)});page.on('console',message=>{if(message.type()==='error')console.error(message.text())});
      const response=await page.goto(fixture.url);
      assert.match(response.headers()['content-security-policy'],/default-src 'self'/);
      await page.getByLabel('Member value').filter({hasText:'1'}).waitFor();
      await page.evaluate(()=>window.updateMember());
      await page.getByLabel('Member value').filter({hasText:'2'}).waitFor();
      await page.locator('.turn-summary').click();
      await page.getByRole('button',{name:'Open external conversation'}).waitFor({state:'visible'});
      await page.locator('.delegation-action').click();
      await page.locator('.source-reference').click();
      assert.deepEqual(await page.evaluate(()=>window.actions),['delegation_open','inspect_source']);
      // Folding alone is not evidence of being away from the scroll tail.
      assert.equal(await page.getByRole('button',{name:'Back to bottom'}).isVisible(),false);
      for(const [button,message] of [['/help','Draft is loading'],['Remove image','Image draft is loading'],['Preview reference','Reference draft is loading']]) {
        await page.getByRole('button',{name:button,exact:true}).click();
        assert.equal(await page.getByLabel('Notice',{exact:true}).textContent(),message);
      }
      assert.equal(await page.evaluate(()=>window.resourceIdentity()),true);
      for(const [seq,key] of [['8',null],['9','next-page']]) {
        await page.evaluate(([seq,key])=>window.updateResource(seq,key),[seq,key]);
        await page.getByRole('button',{name:`Fact ${seq} · text`,exact:true}).waitFor({timeout:3000});
      }
      await page.evaluate(()=>window.imageFrame());
      await page.locator('.image-preview').waitFor();
      const imageURL=await page.locator('.image-preview').getAttribute('src');
      for(let frame=0;frame<12;frame++)await page.evaluate(()=>window.imageFrame());
      assert.equal(await page.evaluate(()=>window.imageReads),1,'Unrelated frames reuse the acquired image');
      assert.equal(await page.locator('.image-preview').getAttribute('src'),imageURL);
      await page.evaluate(()=>window.imageFrame('2'));
      await page.waitForFunction(()=>window.imageReads===2&&window.imageReleases===1);
      await page.evaluate(()=>window.imageFrame('2',true));
      await page.waitForFunction(()=>window.imageReads===3&&window.imageReleases===2);
      await page.evaluate(()=>window.closeImage());
      assert.equal(await page.evaluate(()=>window.imageReleases),3);
      await page.waitForFunction(()=>window.layout.storageNotice.includes('could not be opened'));
      await page.evaluate(()=>window.layout.update({navigationWidth:410,detail:'verbose'}));
      await page.waitForFunction(()=>window.layout.layout.navigationWidth===410);
      await page.evaluate(()=>window.enableStorage());
      await page.waitForFunction(()=>window.layout.storageNotice==='');
      assert.equal(await page.getByLabel('Width',{exact:true}).textContent(),'410');
      assert.equal(await page.evaluate(()=>window.layout.layout.detail),'verbose');
      // A fresh read proves retry saved the pending intents, not just React state.
      assert.equal(await page.evaluate(async()=>{const {PresentationStore}=await import('/presentation-store.js');const store=await PresentationStore.open('review-layout');try{return (await store.load()).navigationWidth}finally{store.close()}}),410);
      const menu=page.locator('details.application-options');
      for(const close of ['escape','outside','action']) {
        await menu.locator('summary').click();
        if(close==='escape')await page.keyboard.press('Escape');
        else await page.getByRole('button',{name:close==='outside'?'Outside':'Menu action',exact:true}).click();
        assert.equal(await menu.getAttribute('open'),null);
      }
      const resize=page.getByRole('separator',{name:'Resize resources'});
      await resize.focus();await page.keyboard.press('ArrowLeft');
      assert.equal(await page.evaluate(()=>window.resized),460,'Keyboard starts at the displayed width');
      const rect=await resize.boundingBox();await page.mouse.move(rect.x+3,rect.y+10);await page.mouse.down();await page.mouse.move(rect.x-37,rect.y+10);await page.mouse.up();
      assert.equal(await page.evaluate(()=>window.resized),490,'Pointer starts at the displayed width');
      const dock=page.getByRole('region',{name:'Dock lifecycle'});
      const strip=dock.locator('[data-dockkit-strip-tabs]');
      const scrolled=await strip.evaluate(node=>{node.style.scrollBehavior='auto';node.scrollLeft=node.scrollWidth;return node.scrollLeft});
      assert(scrolled>0,'The real strip overflows');
      await dock.locator('[data-dockkit-tab]').filter({hasText:'Files 8'}).click();
      assert.equal(await strip.evaluate(node=>node.scrollLeft),scrolled,'Tab selection keeps its pane scroll position');
      assert.equal((await page.evaluate(()=>window.offsetEntries())).length,1,'Live pane retains one offset');
      for(let cycle=0;cycle<24;cycle++)await page.evaluate(()=>window.replaceDock());
      const retained=await page.evaluate(()=>({entries:window.offsetEntries(),pane:window.dockPane()}));
      assert(retained.entries.every(([pane])=>pane===retained.pane),`Removed pane offsets survived: ${retained.entries.length}`);
      const contrast=[];
      for(const theme of ['light','dark']) {
        await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
        const pairs=await page.evaluate(()=>{
          const sample=document.createElement('span');document.body.append(sample);
          const color=(token)=>{sample.style.color='var(--'+token+')';return getComputedStyle(sample).color.match(/[\d.]+/g).slice(0,3).map(Number)};
          const luminance=rgb=>rgb.map(v=>v/255).map(v=>v<=.04045?v/12.92:((v+.055)/1.055)**2.4).reduce((sum,v,i)=>sum+v*[.2126,.7152,.0722][i],0);
          const rows=[['warning','paper'],['warning','warning-bg'],['success','paper'],['teal','paper'],['teal','blue'],['error','paper'],['error','dsw-specific-menu'],['error','dsw-alias-bg-module-platform']].map(([fg,bg])=>{const a=luminance(color(fg)),b=luminance(color(bg));return {fg,bg,ratio:(Math.max(a,b)+.05)/(Math.min(a,b)+.05)}});
          sample.remove();return rows;
        });
        for(const pair of pairs)assert(pair.ratio>=4.5,`${theme} ${pair.fg}/${pair.bg}: ${pair.ratio}`);
        contrast.push({theme,pairs});
        await page.screenshot({path:join(report,`${name}-${theme}.png`),fullPage:true});
      }
      assert.deepEqual(errors,[]);results.push({browser:name,version:browser.version(),contrast,errors,interactions:true,layoutRetry:true,resourceIdentity:true,dockOffsets:true});
    } catch(error) {
      await page?.screenshot({path:join(report,`${name}-failure.png`),fullPage:true}).catch(()=>{});
      await writeFile(join(report,`${name}-failure.txt`),String(error.stack));throw error;
    } finally {await browser.close()}
  }
} finally {await fixture.close();await writeFile(join(report,'result.json'),JSON.stringify(results,null,2))}
