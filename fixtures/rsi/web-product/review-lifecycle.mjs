// Real React effect retirement and IndexedDB checks against current source modules.
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {chromium} from 'playwright';
import {createServer,normalizePath} from '../../../apps/web/node_modules/vite/dist/node/index.js';
const root=resolve(import.meta.dirname,'../../../apps/web'),report=process.env.RSI_REPORT_DIR;
assert(report,'RSI_REPORT_DIR must name an isolated report directory');
const entry=normalizePath(join(root,'review-test.tsx'));
const code=`
import React from 'react';
import {createRoot} from 'react-dom/client';
import {ResourceDocks,ResourceCleanupNotice} from './src/resource-dock.tsx';
import {ResourceOwner} from './src/resource-owner.mjs';
import {installDisclosures} from './disclosure.js';
import {flushSync} from 'react-dom';
import {createSettingsFields} from './src/runtime/settings-fields.js';
import {createSettingsDraft} from './src/runtime/settings-draft.js';
import {RegisteredDraft} from './src/settings/registered.tsx';
import {MAXIMUM_SETTINGS_BYTES} from './settings-json.js';
import {Button} from './src/button.tsx';
import {Notices} from './src/notices.tsx';
import {DeviceStore,applyIntent} from './device-store.js';
import {PresentationStore} from './presentation-store.js';
import {useDeviceNavigation} from './src/navigation-device.ts';
import {emptyDock,dockIntent} from './src/dock-state.ts';
import {presentationIdentity,LayoutContext,usePresentation,useViewport,useNarrow,useResourceVisibility,ResizeHandle} from './src/presentation.tsx';
import {defaultLayout} from './presentation-layout.js';
import {ConversationWidthControls} from './src/conversation-width.tsx';
import {source,installActions} from './src/bridge.ts';
import {resources,resourceHosts,publishResources,clearResources} from './src/resource-hosts.ts';
window.modules={defaultLayout,DeviceStore,emptyDock,dockIntent,resources,resourceHosts,publishResources,clearResources};
window.mountConversationWidth=()=>{
 const node=document.createElement('div');node.style.cssText="position:fixed;inset:0;z-index:9999";document.body.append(node);const root=createRoot(node);window.widthIntents=[];
 function Scene(){const [layout,setLayout]=React.useState({...defaultLayout,conversationWidth:1200}),[container,setContainer]=React.useState(null);
  return <LayoutContext.Provider value={{layout,expand:()=>{},update:patch=>{window.widthIntents.push(patch);setLayout(value=>({...value,...patch}))}}}><section id="width-column" ref={setContainer} style={{width:'100%',height:600,position:'relative'}}><ConversationWidthControls container={container}/></section></LayoutContext.Provider>;
 }
 root.render(<Scene/>);return ()=>{root.unmount();node.remove()};
};

window.settingsChecks=()=>{
 const schema={type:'object',properties:{name:{type:'string'}}},exact='{"unknown":9007199254740993,"negative":-0,"decimal":1.0}';
 const precise=createSettingsFields(schema,exact,false,()=>{});precise.toggle();if(precise.json()!==exact)throw Error('Rejected form switch changed exact JSON');
 const huge=createSettingsFields(schema,JSON.stringify({name:'x'.repeat(MAXIMUM_SETTINGS_BYTES)}),false,()=>{});if(!huge.locked||huge.canSave())throw Error('Initial bound bypass');
 let dirty=0;const editor=createSettingsFields(schema,'{"name":"small"}',false,edited=>dirty+=Number(!!edited));editor.toggle();if(dirty||editor.json()!=='{"name":"small"}')throw Error('Mode switch dirtied JSON');
 editor.editJson('{');if(editor.canSave())throw Error('Invalid JSON enabled Save');
 editor.editJson(JSON.stringify({name:'界'.repeat(MAXIMUM_SETTINGS_BYTES/2)}));if(editor.canSave())throw Error('UTF-8 bound bypass');
 let refused=false;try{editor.json()}catch{refused=true}if(!refused)throw Error('Oversized submission admitted');
 editor.editJson('{"name":"restored"}');editor.toggle();if(!editor.canSave()||JSON.parse(editor.json()).name!=='restored')throw Error('Reduced edit did not recover');
 const readonly=createSettingsFields(schema,'{"name":"fixed"}',true,()=>{});readonly.toggle();readonly.toggle();if(!readonly.locked||readonly.canSave())throw Error('Rebuilt fields escaped read-only state');
 return {exactJson:true,initialBound:true,utf8EditBound:true,recovery:true,readOnlyRebuild:true};
};
window.settingsDraftChecks=async()=>{
 const calls=[],drafts=new Map();let settle;
 const command=value=>{calls.push(value);return new Promise(resolve=>settle=resolve)};
 const editor=(namespace,ticket,revision='1')=>({namespace,ticket,version:['scope',revision],text:'{"name":"saved"}',description:{writable:true,defaults:{},metadata:{schema:{type:'object',properties:{name:{type:'string'}}},sensitive_fields:[],applies:'live',description:'Draft fixture'}}});
 const a=editor('rsi.client','a'),b=editor('rsi.agent','b'),host=document.createElement('section');document.body.append(host);const root=createRoot(host);let current;
 const show=value=>{flushSync(()=>{current=drafts.get(value.namespace);if(!current){current=createSettingsDraft(value,command);drafts.set(value.namespace,current)}else current.update(value);root.render(<RegisteredDraft draft={current}/>)});return host.querySelector('form')};
 const update=work=>flushSync(work),done=async()=>{settle();await new Promise(resolve=>setTimeout(resolve,0))};
 const readyFocus=stage=>{if(!host.contains(document.activeElement)||!document.activeElement.matches('button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled)'))throw Error('Settings lost ready focus at '+stage+': '+document.activeElement.outerHTML.slice(0,160))};
 try{
  let form=show(a);update(()=>current.fields.toggle());let text=form.querySelector('textarea.settings-text');const exact='{"large":9007199254740993,"negative":-0,"decimal":1.0}';
  update(()=>current.fields.editJson(exact));show(b);form=show({...a,ticket:'a2'});if(form.querySelector('textarea.settings-text').value!==exact)throw Error('Section return discarded exact draft');
  form=show({...a,ticket:'a3',version:['scope','2']});if(!form.querySelector('[type=submit]').disabled||current.fields.json()!==exact)throw Error('Stale draft silently rebased');
  show({...a,ticket:'reconciled'});if(current.error||!current.canSave)throw Error('Reconciled draft kept a stale conflict');
  form=show({...a,ticket:'a3',version:['scope','2']});const discard=[...form.querySelectorAll('button')].find(button=>button.textContent==='Discard edits and reload');discard.focus();update(()=>discard.click());
  show({...a,ticket:'a4',version:['scope','2']});await done();if(!current.canSave)throw Error('Explicit reload did not recover');readyFocus('discard');
  update(()=>current.fields.edit(current.fields.fields.parts[0].field,'changed'));form.querySelector('[type=submit]').focus();update(()=>form.requestSubmit());if(!form.querySelector('fieldset').disabled)throw Error('Save allowed edits during publication');
  show({...a,ticket:'a5',version:['scope','3']});await done();if(!current.canSave)throw Error('Frame-before-ack save did not recover');readyFocus('frame-before-ack');
  update(()=>form.requestSubmit());await done();if(current.canSave||!current.error.includes('Waiting for refreshed settings'))throw Error('Acknowledged save reused old ticket');
  const recover=[...form.querySelectorAll('button')].find(button=>button.textContent==='Reload settings');if(!recover||recover.disabled)throw Error('Waiting save has no recovery');update(()=>recover.click());await done();
  if(calls.at(-1).action!=='settings_read'||current.canSave)throw Error('Recovery replayed save');show({...a,ticket:'a6',version:['scope','4']});if(!current.canSave)throw Error('Ack-before-frame save did not recover');readyFocus('ack-before-frame');
  update(()=>current.fields.edit(current.fields.fields.parts[0].field,'draft'));show({...a,ticket:'a7',version:['scope','5']});update(()=>current.reload());await done();if(current.canSave)throw Error('Discard unlocked before refresh');
  show({...a,ticket:'a8',version:['scope','5']});if(JSON.parse(current.fields.json()).name!=='saved'||!current.canSave)throw Error('Same-version discard retained edits');
  return {exactDraft:true,versionConflict:true,explicitDiscard:true,saveFrameOrders:true,focusRetained:true,conflictCleared:true,calls:calls.map(call=>call.action)};
 }finally{root.unmount();host.remove();for(const draft of drafts.values())draft.retire()}
};
window.mountCleanup=async()=>{
 const owner=new ResourceOwner(async()=>{throw Error('Input is busy')});
 owner.reservePanel('opaque-session',{view:'1',open_revision:'2'},'Workspace files').commit();
 await new Promise(resolve=>setTimeout(resolve,0));
 installActions({resources:()=>owner});source.set({surfaces:{},application_surfaces:[]});
 const node=document.createElement('section');document.body.append(node);const root=createRoot(node);
 root.render(<ResourceCleanupNotice/>);window.unmountCleanup=()=>{root.unmount();node.remove()};
};
window.mountCleanupFlood=async()=>{
 await import('./styles.css');const shell=(await import('./src/shell.module.css')).default;
 window.cleanupCalls=0;
 const owner=new ResourceOwner(async()=>{window.cleanupCalls++;throw Error('Input is busy. '+ '原始资源仍等待清理。'.repeat(8))});
 for(let i=0;i<64;i++)owner.reservePanel('session-'+Math.floor(i/16),{view:String(i+1),open_revision:'2'},'Workspace files '+(i+1)).commit();
 await new Promise(resolve=>setTimeout(resolve,0));
 installActions({resources:()=>owner});source.set({surfaces:{},application_surfaces:[]});
 const node=document.createElement('div');node.id='root';node.className=shell.scope;document.body.append(node);const root=createRoot(node);
 root.render(<><header><strong>RSIversi · cleanup recovery</strong></header><ResourceCleanupNotice/><main id="workbench" className="workbench navigation-hidden resources-closed"><section style={{gridColumn:2,minHeight:0,padding:16,display:'flex',flexDirection:'column'}}><h1>Conversation remains available</h1><p>64 retained cleanup failures, four Sessions. Retry controls stay inside their own scroll area.</p><label style={{marginTop:'auto'}}>Main message<textarea aria-label="Main message" placeholder="Continue the conversation"/></label></section></main></>);
 window.unmountCleanupFlood=()=>{root.unmount();node.remove()};
};
window.mountDisclosures=()=>{
 const node=document.createElement('section');document.body.append(node);const root=createRoot(node),stop=installDisclosures(document);
 root.render(<><details className="session-options"><summary>Conversation options</summary><Button onClick={()=>{node.querySelector('output').textContent='action received'}}>Pin conversation</Button><Button onClick={()=>new Promise(resolve=>{window.finishMenuAction=resolve})}>Slow action</Button><Button onClick={async()=>{throw Error("Action failed")}}>Failing action</Button><select aria-label="Menu selection" defaultValue="a"><option value="a">A</option><option value="b">B</option></select></details><Notices/><input aria-label="Outside menu"/><output>no action</output><div className="mode-verbose"><details className="source-menu" open><summary style={{display:'none'}}>Inline sources</summary><Button onClick={()=>{node.querySelector('output').textContent='inline one'}}>Inline one</Button><Button onClick={()=>{node.querySelector('output').textContent='inline two'}}>Inline two</Button></details></div><dialog id="verbose-dialog"><button>Modal action</button></dialog></>);
 return ()=>{stop();root.unmount();node.remove()};
};
window.stalePresentationReply=async(mode)=>{
 const original=PresentationStore.open,actEnvironment=window.IS_REACT_ACT_ENVIRONMENT;
 window.IS_REACT_ACT_ENVIRONMENT=true;
 let saved=defaultLayout,current,notify,readReply,failedSave,pauseRead=false;const writes=[];
 const store={load:()=>{const value=saved;return pauseRead?new Promise(resolve=>{readReply=()=>resolve(value)}):Promise.resolve(value)},apply:intent=>new Promise((resolve,reject)=>writes.push({resolve,reject})).then(()=>{saved=applyIntent('layouts',saved,intent);return saved}),subscribe:callback=>{notify=callback;return()=>{}},close(){}};
 PresentationStore.open=async()=>store;
 presentationIdentity.set('presentation-race:'+mode);source.set({surfaces:{},application_surfaces:[]});
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);
 function Probe(){current=usePresentation();return <output>{current.layout.navigationWidth}</output>}
 try{
  await React.act(async()=>root.render(<Probe/>));
  if(mode==='read'){
   pauseRead=true;await React.act(async()=>notify());
   await React.act(async()=>current.update({navigationWidth:310}));
   await React.act(async()=>readReply());
  }else if(mode==='reload'){
   pauseRead=true;await React.act(async()=>notify());
   saved=applyIntent('layouts',saved,{kind:'patch',patch:{navigationWidth:310}});
   await React.act(async()=>notify());pauseRead=false;
   await React.act(async()=>readReply());
  }else{
   await React.act(async()=>current.update({navigationWidth:300}));
   await React.act(async()=>current.update({navigationWidth:310}));
   await React.act(async()=>mode==='write-error'?writes[0].reject(Error('old save failed')):writes[0].resolve());
   if(mode==='write-error'){
    failedSave={width:current.layout.navigationWidth,notice:current.storageNotice,persisted:saved.navigationWidth,writes:writes.length};
    await React.act(async()=>current.retry());
    if(writes.length!==2)throw Error('Explicit retry did not submit the pending layout intent');
    await React.act(async()=>writes[1].resolve());
   }
  }
  return {width:current.layout.navigationWidth,notice:current.storageNotice,persisted:saved.navigationWidth,failedSave};
 }finally{
  for(const write of writes)await React.act(async()=>write.resolve());
  await React.act(async()=>readReply?.());
  await React.act(async()=>root.unmount());node.remove();PresentationStore.open=original;window.IS_REACT_ACT_ENVIRONMENT=actEnvironment;
 }
};
window.staleNavigationRead=async(kind)=>{
 const identity='navigation-race:'+kind,scope=kind==='sessionOrder'?'sessions:all':'workspaces',store=await DeviceStore.open(identity);
 const members=[{id:'session-a',partition:'group'}];
 await store.applyAll([{bucket:'orders',scope,intent:{kind:'reconcile',members}},{bucket:'preferences',scope:'',intent:{kind:'patch',patch:{[kind]:'manual'}}}]);
 let release,entered;const gate=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const read=DeviceStore.prototype.read;let held=false;
 DeviceStore.prototype.read=async function(bucket,scope=''){
  const result=await read.call(this,bucket,scope);
  if(this!==store&&this.key===identity&&bucket==='preferences'&&!held){held=true;entered();await gate}
  return result;
 };
 const nav={ticket:'navigation',metadata_revision:'1',filter:{query:'',archived:false},order:{ticket:'order',requested:['session-a'],entries:[],seed:{scope:{kind:'all'},metadata_revision:'1',membership:{kind:'available',members:[{session:'session-a',group:'group',pinned:false,archived:false,last_activity_ms:'1',workspace:null}]}}}};
 source.set({navigation:nav,surfaces:{},application_surfaces:[]});presentationIdentity.set(identity);
 installActions({command:async()=>{},call:async()=>'',open:async()=>{},select(){},closeSurface:async()=>{},addSurface:async()=>{}});
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);
 const workspaces=[{id:'workspace-a',path:'/workspace',location:{kind:'local'}}];
 function Probe(){const device=useDeviceNavigation(nav,workspaces,false);return <output data-ready={device.ready}>{device.preferences[kind]}</output>}
 try{
  root.render(<Probe/>);await started;
  await store.applyAll([{bucket:'orders',scope,intent:{kind:'updated'}},{bucket:'preferences',scope:'',intent:{kind:'patch',patch:{[kind]:'updated'}}}]);
  release();
  const deadline=performance.now()+10000;
  while(performance.now()<deadline&&(node.querySelector('output')?.dataset.ready!=='true'||node.querySelector('output')?.textContent!=='updated'))await new Promise(resolve=>setTimeout(resolve,10));
  if(node.querySelector('output')?.dataset.ready!=='true')throw Error('Restored navigation never became ready');
  return {preferences:(await read.call(store,'preferences')).value,order:(await read.call(store,'orders',scope)).value,displayed:node.querySelector('output').textContent};
 }finally{release();root.unmount();node.remove();DeviceStore.prototype.read=read;store.close()}
};
window.mountResize=()=>{
 const node=document.createElement('main');node.id='workbench';document.body.append(node);const root=createRoot(node);
 function Resizer(){const viewport=useViewport();const [ratio,setRatio]=React.useState(.45);return <section style={{width:ratio*viewport.width,height:40,position:'relative'}}><output>{ratio}</output><ResizeHandle name="resources" value={ratio*viewport.width} min={300} max={viewport.width*.7} reverse onChange={value=>setRatio(value/viewport.width)}/></section>}
 root.render(<Resizer/>);return ()=>{root.unmount();node.remove()};
};
window.mountVisibility=()=>{
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);window.visibilityWrites=[];
 function Visibility(){
  const narrow=useNarrow();const [saved,setSaved]=React.useState(defaultLayout);
  const {layout,update}=useResourceVisibility(saved,patch=>{window.visibilityWrites.push(patch);setSaved(value=>({...value,...patch}))},narrow);
  React.useEffect(()=>{window.oldVisibilityCallback=update},[]);
  return <output data-narrow={narrow} data-closed={layout.resourcesClosed} data-saved={saved.resourcesClosed}/>;
 }
 root.render(<Visibility/>);window.unmountVisibility=()=>{root.unmount();node.remove()};
};
window.lifecycle=async(stage)=>{
 const events={reads:0,subscribed:0,closed:0,focus:0,applies:0,detaches:0};let release,entered;
 const gate=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const pause=async name=>{if(stage===name){entered();await gate}};
 const saved=dockIntent(emptyDock(),{kind:'dock',operation:'open',coordinate:stage==='terminal'?{action:'resources'}:{action:'ui_block',key:'example'},title:'Example'});
 const original=DeviceStore.open;
 DeviceStore.open=async()=>{await pause('open');return {read:async()=>{events.reads++;await pause('read');return {value:saved}},apply:async()=>{events.applies++;return {value:saved}},subscribe:()=>{events.subscribed++;return()=>events.subscribed--},close:()=>events.closed++}};
 const add=window.addEventListener,remove=window.removeEventListener;
 const listeners=new Set();window.addEventListener=function(type,fn,...args){if(type==='focus')listeners.add(fn);return add.call(this,type,fn,...args)};
 window.removeEventListener=function(type,fn,...args){if(type==='focus')listeners.delete(fn);return remove.call(this,type,fn,...args)};
 const resourceOwner=new ResourceOwner(async(method,payload)=>{const data=JSON.parse(payload);if(method==='resource_open'){await pause('reopen');return JSON.stringify({status:'opened',request:data.opening_request,view:'1',open_revision:'1'})}if(method==='command')return '{}';const request=data.request;if(request.type==='create'){await pause('terminal');return JSON.stringify({value:{id:'attachment',terminal:{id:'created'}}})}if(request.type==='detach')events.detaches++;return JSON.stringify({value:[]})});
 installActions({resources:()=>resourceOwner,command:async()=>{},call:resourceOwner.call,open:async()=>{},select(){},closeSurface:async()=>{},addSurface:async()=>{}});
 presentationIdentity.set('lifecycle:'+stage);
 source.set({surfaces:{main:{kind:'native',session:'test',generation:'1',ui_surfaces:[]}},application_surfaces:[]});
 const node=document.createElement('div');document.body.append(node);const root=createRoot(node);root.render(<LayoutContext.Provider value={{layout:{...defaultLayout,resourcesClosed:false},update(){},expand(){}}}><ResourceDocks launcher={<span>Resources</span>}/></LayoutContext.Provider>);
 if(stage==='terminal'){for(let attempt=0;attempt<100&&!node.querySelector('.terminal-list');attempt++)await new Promise(resolve=>setTimeout(resolve,10));[...node.querySelectorAll('button')].find(button=>button.textContent==='New terminal').click()}
 await Promise.race([started,new Promise((_,reject)=>setTimeout(()=>reject(Error("lifecycle did not reach "+stage)),10000))]);root.unmount();release();await new Promise(resolve=>setTimeout(resolve,30));
 window.dispatchEvent(new Event('focus'));await new Promise(resolve=>setTimeout(resolve,30));
 events.focus=listeners.size;DeviceStore.open=original;window.addEventListener=add;window.removeEventListener=remove;node.remove();return events;
};
window.mountExtension=async(action,seed)=>{
 const identity='extension:'+action,scope='dock:extension-session';
 if(seed){const store=await DeviceStore.open(identity);await store.apply('layouts',{kind:'dock',operation:'open',coordinate:{action,bundle:'fixture',name:'card'},title:'Fixture card'},scope);store.close()}
 const reference={name:'card',revision:'fixture'};
 window.extensionCommands=[];
 const resourceOwner=new ResourceOwner(async(method,text)=>{const request=JSON.parse(text);if(method==='resource_open'){window.extensionCommands.push(request.command);return JSON.stringify({status:'opened',request:request.opening_request,view:String(window.extensionCommands.length),open_revision:String(window.extensionCommands.length)})}return JSON.stringify({value:[]})});
 installActions({resources:()=>resourceOwner,command:async()=>{},call:resourceOwner.call,open:async()=>{},select(){},closeSurface:async()=>{},addSurface:async()=>{}});
 presentationIdentity.set(identity);
 const item={bundle:'fixture',reference,title:'Fixture card'};
 source.set({surfaces:{main:{kind:'native',session:'extension-session',generation:'17',ui_surfaces:[item]}},application_surfaces:[item]});
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);
 root.render(<LayoutContext.Provider value={{layout:{...defaultLayout,resourcesClosed:false},update(){},expand(){}}}><ResourceDocks launcher={<span>Resources</span>}/></LayoutContext.Provider>);
 window.unmountExtension=()=>{root.unmount();node.remove()};
};
if(new URLSearchParams(location.search).has('cleanup'))await window.mountCleanupFlood();
`;
const server=await createServer({configFile:false,root,cacheDir:join(root,'node_modules/.vite-review-fixture'),optimizeDeps:{entries:[],noDiscovery:true,include:['react','react-dom','react-dom/client','react/jsx-runtime','react/jsx-dev-runtime','use-sync-external-store/shim','use-sync-external-store/shim/with-selector','clsx','@xterm/xterm','@xterm/addon-fit']},esbuild:{jsx:'automatic'},server:{host:'127.0.0.1',port:0},plugins:[{name:'review-fixture',resolveId(id){if(id==='/review-test.tsx')return entry},load(id){if(id===entry)return code},configureServer(server){server.middlewares.use('/review.html',(_req,res)=>{res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Review lifecycle</title><script type="module" src="/review-test.tsx"></script>')})}}]});
let browser;const errors=[],checks=[];
if(process.env.RSI_REVIEW_PREVIEW){
 await server.listen();console.log(`http://127.0.0.1:${server.httpServer.address().port}/review.html?cleanup=1`);
 await new Promise(resolve=>{process.once('SIGINT',resolve);process.once('SIGTERM',resolve)});await server.close();
}else{
try{
 await mkdir(report,{recursive:true});await server.listen();browser=await chromium.launch();
 const page=await browser.newPage();page.on('pageerror',e=>errors.push(e.message));page.on('console',event=>{if(event.type()==='error')errors.push(event.text())});await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/review.html`);await page.waitForFunction(()=>window.lifecycle);
 checks.push({settingsEditor:await page.evaluate(()=>window.settingsChecks()),settingsDrafts:await page.evaluate(()=>window.settingsDraftChecks())});
 for(const mode of ['write','read','write-error','reload']){
  const presentation=await page.evaluate(mode=>window.stalePresentationReply(mode),mode);
  assert.equal(presentation.width,310);assert.equal(presentation.notice,'');
  if(mode==='write-error'){
   assert.equal(presentation.failedSave.width,310);assert.match(presentation.failedSave.notice,/could not be saved.*Retry layout/);
   assert.equal(presentation.failedSave.persisted,280);assert.equal(presentation.failedSave.writes,1);assert.equal(presentation.persisted,310);
  }
  checks.push({stalePresentationReply:mode,...presentation});
 }
 for(const kind of ['sessionOrder','workspaceOrder']){
  const navigation=await page.evaluate(kind=>window.staleNavigationRead(kind),kind);
  assert.equal(navigation.preferences[kind],'updated');assert.equal(navigation.displayed,'updated');assert.deepEqual(navigation.order.ids,[]);checks.push({staleNavigationRead:kind,...navigation});
 }
 await page.setViewportSize({width:1440,height:900});await page.evaluate(()=>window.mountVisibility());
 await page.waitForFunction(()=>window.oldVisibilityCallback);
 await page.setViewportSize({width:390,height:844});await page.waitForFunction(()=>document.querySelector('output')?.dataset.narrow==='true');
 await page.evaluate(()=>window.oldVisibilityCallback({resourcesClosed:false}));
 await page.waitForFunction(()=>document.querySelector('output')?.dataset.closed==='false');
 assert.deepEqual(await page.evaluate(()=>window.visibilityWrites),[]);
 await page.setViewportSize({width:1440,height:900});await page.waitForFunction(()=>document.querySelector('output')?.dataset.narrow==='false');
 assert.equal(await page.locator('output').getAttribute('data-closed'),'true');
 await page.evaluate(()=>window.oldVisibilityCallback({resourcesClosed:false}));
 await page.waitForFunction(()=>document.querySelector('output')?.dataset.saved==='false');
 assert.deepEqual(await page.evaluate(()=>window.visibilityWrites),[{resourcesClosed:false}]);
 await page.evaluate(()=>window.unmountVisibility());checks.push({retainedResourceListenerUsesCurrentViewport:true});
 for(const action of ['application_surface','surface']){
  const expected=action==='application_surface'?{action:'application_ui_surface',reference:{name:'card',revision:'fixture'}}:{action:'ui_surface',reference:{name:'card',revision:'fixture'},pane:'main',generation:'17'};
  await page.evaluate(action=>window.mountExtension(action,true),action);
  await page.waitForFunction(()=>window.extensionCommands.length===1);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected]);
  await page.locator('[data-dockkit-tab]').first().click({button:'right'});
  await page.getByRole('menuitem',{name:'Duplicate',exact:true}).click();
  await page.waitForFunction(()=>window.extensionCommands.length===2);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected,expected]);
  await page.evaluate(()=>window.unmountExtension());
  await page.evaluate(action=>window.mountExtension(action,false),action);
  await page.waitForFunction(()=>window.extensionCommands.length===2);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected,expected]);
  await page.evaluate(()=>window.unmountExtension());
  checks.push({action,restore:2,duplicate:true,exactCommand:expected});
 }
 for(const stage of ['open','read','reopen','terminal']){
  const result=await page.evaluate(stage=>window.lifecycle(stage),stage);
  assert.equal(result.subscribed,0);assert.equal(result.focus,0);assert.equal(result.closed,1);assert.equal(result.reads,stage==='open'?0:1);assert.equal(result.applies,0);assert.equal(result.detaches,stage==='terminal'?1:0);checks.push({stage,...result});
 }
 const hosts=await page.evaluate(()=>{
  const {resourceHosts,resources,publishResources,clearResources}=modules;let changes=0;
  const stop=resources.subscribe(()=>changes++),host={body:document.createElement('div'),title:'Files',entry:{view:'one',session:'test',pane:'main',generation:'1',floating:false,reopen:{action:'ui_block',key:'key'}},key:'first'};
  resourceHosts.set('one',host);publishResources();const snapshot=resources.getSnapshot();
  for(let i=0;i<100;i++)publishResources();
  const stable=resources.getSnapshot()===snapshot,contentChanges=changes;
  host.key='second';publishResources();host.model={kind:'ui',data:{error:'Fixture detail'},command:async()=>{}};publishResources();
  host.title='New title';publishResources();host.entry={...host.entry,floating:true};publishResources();clearResources();stop();return {stable,contentChanges,changes};
 });assert.deepEqual(hosts,{stable:true,contentChanges:1,changes:6});checks.push(hosts);
 const storage=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock}=modules,a=await DeviceStore.open('review'),b=await DeviceStore.open('review');
  await Promise.all([a.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'resources'},title:'Resources'},'dock:session'),b.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'ui_block',key:'one'},title:'One'},'dock:session')]);
  const before=await a.read('layouts','dock:session');
  let puts=0,deletes=0,clears=0;const put=IDBObjectStore.prototype.put,del=IDBObjectStore.prototype.delete,clear=IDBObjectStore.prototype.clear;
  IDBObjectStore.prototype.put=function(...args){puts++;return put.apply(this,args)};IDBObjectStore.prototype.delete=function(...args){deletes++;return del.apply(this,args)};IDBObjectStore.prototype.clear=function(...args){clears++;return clear.apply(this,args)};
  await a.apply('layouts',{kind:'dock',operation:'mode',mode:'fullscreen'},'dock:session');
  IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.delete=del;IDBObjectStore.prototype.clear=clear;
  const after=await b.read('layouts','dock:session');a.close();b.close();return {tabs:Object.keys(before.value.state.tabs).length,before:before.revision,after:after.revision,puts,deletes,clears,mode:after.value.state.mode};
 });assert.deepEqual(storage,{tabs:2,before:'2',after:'3',puts:1,deletes:0,clears:0,mode:'fullscreen'});checks.push(storage);
 const noops=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock,dockIntent}=modules,a=await DeviceStore.open('noops'),b=await DeviceStore.open('noops'),scope='dock:capped';
  const act=(doc,operation,extra={})=>dockIntent(doc,{kind:'dock',operation,...extra});
  let doc=emptyDock();
  for(let i=0;i<6;i++)doc=act(doc,'open',{coordinate:{action:'resources'},title:`Resource ${i}`,duplicate:true});
  doc=act(doc,'split',{pane:doc.state.activePaneId});
  for(const tab of Object.keys(doc.state.tabs).slice(0,4))doc=act(doc,'float',{tab});
  await new Promise((resolve,reject)=>{const tx=a.database.transaction('layouts','readwrite');tx.objectStore('layouts').put({key:a.scopedKey(scope),value:doc,revision:'7',used:1});tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error)});
  const before=await b.read('layouts',scope),tab=Object.keys(doc.state.tabs).at(-1);
  let puts=0,deletes=0,broadcasts=0,notifications=0;
  const put=IDBObjectStore.prototype.put,del=IDBObjectStore.prototype.delete,post=BroadcastChannel.prototype.postMessage;
  const stops=[a.subscribe('layouts',scope,()=>notifications++),b.subscribe('layouts',scope,()=>notifications++)];
  IDBObjectStore.prototype.put=function(...args){puts++;return put.apply(this,args)};
  IDBObjectStore.prototype.delete=function(...args){deletes++;return del.apply(this,args)};
  BroadcastChannel.prototype.postMessage=function(...args){broadcasts++;return post.apply(this,args)};
  let empty;
  try{
   empty=await a.apply('layouts',{kind:'dock',operation:'undo'},'dock:absent');
   await a.apply('layouts',{kind:'dock',operation:'redo'},'dock:absent');
   await a.apply('layouts',{kind:'dock',operation:'split',pane:doc.state.activePaneId},scope);
   await a.apply('layouts',{kind:'dock',operation:'float',tab},scope);
   await a.apply('layouts',{kind:'dock',operation:'drop',tab,pane:doc.state.activePaneId,zone:'outside'},scope);
  }finally{IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.delete=del;BroadcastChannel.prototype.postMessage=post}
  const after=await b.read('layouts',scope);await new Promise(resolve=>setTimeout(resolve,20));
  stops.forEach(stop=>stop());a.close();b.close();
  return {puts,deletes,broadcasts,notifications,emptyRevision:empty.revision,revision:after.revision,unchanged:JSON.stringify(before)===JSON.stringify(after)};
 });assert.deepEqual(noops,{puts:0,deletes:0,broadcasts:0,notifications:0,emptyRevision:'0',revision:'7',unchanged:true});checks.push(noops);
 await writeFile(join(report,'dock-noops.json'),JSON.stringify(noops,null,2));
 const history=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock,dockIntent}=modules,a=await DeviceStore.open('history'),b=await DeviceStore.open('history'),scope='dock:session';
  let doc=emptyDock();
  for(let i=0;i<16;i++)doc=dockIntent(doc,{kind:'dock',operation:'open',coordinate:{action:'resources'},title:`Resource ${i}`,duplicate:true});
  for(let i=0;i<64;i++)doc=dockIntent(doc,{kind:'dock',operation:'mode',mode:i%2?'push':'fullscreen'});
  const write=record=>new Promise((resolve,reject)=>{const tx=b.database.transaction('layouts','readwrite');tx.objectStore('layouts').put(record);tx.oncomplete=resolve;tx.onerror=tx.onabort=()=>reject(tx.error)});
  await write({key:a.scopedKey(scope),value:doc,revision:'1',used:1});
  const intent={kind:'dock',operation:'mode',mode:'fullscreen'},clone=globalThis.structuredClone;
  let copies=0,expected,once,actual;
  globalThis.structuredClone=(...args)=>{copies++;return clone(...args)};
  try{expected=dockIntent(doc,intent);once=copies;copies=0;actual=(await a.apply('layouts',intent,scope)).value}
  finally{globalThis.structuredClone=clone}
  const equal=JSON.stringify(actual)===JSON.stringify(expected),saved=await b.read('layouts',scope);
  const timing=[];
  for(let i=0;i<30;i++){const start=performance.now();await a.apply('layouts',{...intent,mode:i%2?'push':'fullscreen'},scope);if(i>=5)timing.push(performance.now()-start)}
  timing.sort((x,y)=>x-y);
  const bad=clone(saved.value);bad.history.entries[0].inverse=[{type:'openTab',paneId:bad.state.activePaneId,tab:{id:'tab999',kind:'rsi',contentId:'null',title:'bad'},index:0}];
  await write({key:b.scopedKey(scope),value:bad,revision:'99',used:2});
  let rejected=false;try{await a.apply('layouts',intent,scope)}catch(error){rejected=String(error).includes('Invalid resource layout')}
  const retained=await new Promise((resolve,reject)=>{const tx=b.database.transaction('layouts','readonly'),read=tx.objectStore('layouts').get(b.scopedKey(scope));tx.oncomplete=()=>resolve(read.result.revision);tx.onerror=()=>reject(tx.error)});
  a.close();b.close();return {tabs:Object.keys(doc.state.tabs).length,history:doc.history.entries.length,once,copies,equal,rejected,retained,p50ms:timing[12],p95ms:timing[23]};
 });checks.push(history);
 await writeFile(join(report,'history.json'),JSON.stringify(history,null,2));
 assert.equal(history.tabs,16);assert.equal(history.history,64);assert.equal(history.copies,history.once);assert.equal(history.equal,true);assert.equal(history.rejected,true);assert.equal(history.retained,'99');

 const oversized=await page.evaluate(async()=>{
  const owner=await modules.DeviceStore.open('oversized'),scope='dock:large';
  let saved=await owner.read('layouts',scope),accepted=0,rejected=false;
  for(let i=0;i<16;i++){
   try{saved=await owner.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'inspect_source',source:{payload:'x'.repeat(12000),id:i}},title:'Large source',duplicate:true},scope);accepted++}
   catch(error){if(!String(error).includes('too large'))throw error;rejected=true;break}
  }
  const after=await owner.read('layouts',scope),preserved=JSON.stringify(saved)===JSON.stringify(after);
  const tab=Object.keys(after.value.state.tabs)[0];
  const closed=await owner.apply('layouts',{kind:'dock',operation:'close',tab},scope);
  owner.close();return {accepted,rejected,preserved,closedTabs:Object.keys(closed.value.state.tabs).length};
 });assert.equal(oversized.rejected,true);assert.equal(oversized.preserved,true);assert.equal(oversized.closedTabs,oversized.accepted-1);checks.push({oversized});
 for(const legacy of ['valid','invalid','over-count']){
  const migration=await browser.newPage();
  await migration.goto(`http://127.0.0.1:${server.httpServer.address().port}/review.html`);await migration.waitForFunction(()=>window.modules);
  const result=await migration.evaluate(async legacy=>{
   const records=Array.from({length:legacy==='over-count'?65:1},(_,i)=>({key:i===0?'legacy':`legacy-${i}`,layout:modules.defaultLayout,used:legacy==='invalid'?-1:1}));
   await new Promise((resolve,reject)=>{
    const opening=indexedDB.open('rsi.presentation',1);opening.onupgradeneeded=()=>opening.result.createObjectStore('layouts',{keyPath:'key'});opening.onerror=()=>reject(opening.error);
    opening.onsuccess=()=>{const db=opening.result,tx=db.transaction('layouts','readwrite');for(const record of records)tx.objectStore('layouts').put(record);tx.oncomplete=()=>{db.close();resolve()};tx.onabort=()=>reject(tx.error)};
   });
   const owner=await modules.DeviceStore.open('legacy'),before=await owner.read('layouts');
   const removed=!owner.database.objectStoreNames.contains('legacy-layouts-v1');
   const saved=await owner.apply('layouts',{kind:'patch',patch:{navigationWidth:311}});owner.close();
   const reopened=await modules.DeviceStore.open('legacy'),after=await reopened.read('layouts');reopened.close();
   return {revision:before.revision,removed,after:after.revision,saved:saved.revision,width:after.value.navigationWidth};
  },legacy);
  assert.equal(result.revision,'0');assert.equal(result.removed,true);assert.equal(result.after,result.saved);assert.equal(result.width,311);checks.push({legacy,...result});await migration.close();
 }
 await page.setViewportSize({width:1400,height:900});
 await page.evaluate(()=>{window.unmountWidth=window.mountConversationWidth()});
 const widthHandle=page.getByRole('separator',{name:'Resize conversation (right)'});
 try{await widthHandle.waitFor({timeout:5000})}catch(error){await mkdir(report,{recursive:true});await writeFile(join(report,'width-failure.json'),JSON.stringify(await page.evaluate(()=>({html:document.querySelector('#width-column')?.outerHTML,rect:document.querySelector('#width-column')?.getBoundingClientRect().toJSON(),text:document.body.innerText})),null,2));throw error}
 const widthValue=()=>page.locator('#width-column').evaluate(node=>parseFloat(node.style.getPropertyValue('--conversation-width')));
 assert.equal(await widthValue(),1200);
 await widthHandle.click();assert.equal(await page.evaluate(()=>widthIntents.length),0);
 await page.setViewportSize({width:1000,height:800});
 await page.waitForFunction(()=>parseFloat(document.querySelector('#width-column').style.getPropertyValue('--conversation-width'))===1000);
 assert.equal(await page.evaluate(()=>widthIntents.length),0);
 await page.setViewportSize({width:1400,height:900});await page.waitForFunction(()=>document.querySelector('#width-column').style.getPropertyValue('--conversation-width')==='1200px');
 await widthHandle.focus();await widthHandle.press('Home');assert.equal(await widthValue(),640);
 await widthHandle.press('ArrowRight');assert.equal(await widthValue(),650);
 const widthBox=await widthHandle.boundingBox();await page.mouse.move(widthBox.x+widthBox.width/2,widthBox.y+20);await page.mouse.down();await page.mouse.move(widthBox.x+widthBox.width/2+40,widthBox.y+20);await page.mouse.up();
 await page.waitForFunction(()=>widthIntents.at(-1).conversationWidth===730);
 const committed=await page.evaluate(()=>widthIntents.length);
 const cancelBox=await widthHandle.boundingBox();await page.mouse.move(cancelBox.x+cancelBox.width/2,cancelBox.y+20);await page.mouse.down();await page.mouse.move(cancelBox.x+cancelBox.width/2+25,cancelBox.y+20);await widthHandle.press('Escape');await page.mouse.up();
 assert.equal(await page.evaluate(()=>widthIntents.length),committed);assert.equal(await widthValue(),730);
 await page.setViewportSize({width:390,height:844});await widthHandle.waitFor({state:'detached'});assert.equal(await page.evaluate(()=>widthIntents.length),committed);
 await page.evaluate(()=>unmountWidth());checks.push({conversationWidth:{keyboard:true,pointer:true,cancel:true,pressOnlyPreservesPreference:true,viewportDoesNotPersist:true}});
 await page.setViewportSize({width:1400,height:900});
 await page.evaluate(()=>{window.unmountResize=window.mountResize()});
 const handle=page.getByRole('separator',{name:'Resize resources'});
 await page.waitForFunction(()=>document.querySelector('[aria-label="Resize resources"]')?.getAttribute('aria-valuenow')==='630');
 await page.setViewportSize({width:1200,height:800});
 await page.waitForFunction(()=>document.querySelector('[aria-label="Resize resources"]')?.getAttribute('aria-valuenow')==='540');
 await handle.focus();await page.keyboard.press('ArrowLeft');
 await page.waitForFunction(()=>Math.abs(Number(document.querySelector('output').textContent)-550/1200)<1e-9);
 checks.push({viewportResize:{before:630,after:540,keyboard:550}});
 await page.evaluate(()=>window.unmountResize());
 await page.evaluate(()=>{window.unmountDisclosures=window.mountDisclosures()});
 const menu=page.locator('details.session-options'),summary=menu.locator('summary');
 await summary.click();await menu.getByRole('button',{name:'Pin conversation',exact:true}).click();
 assert.equal(await page.locator('output').innerText(),'action received');
 assert.equal(await menu.getAttribute('open'),null);
 await summary.click();await summary.press('Escape');assert.equal(await menu.getAttribute('open'),null);
 assert(await summary.evaluate(node=>node===document.activeElement));
 await summary.click();await page.getByRole('textbox',{name:'Outside menu'}).focus();assert.equal(await menu.getAttribute('open'),null);
 await page.evaluate(()=>{document.activeElement.blur();document.querySelector('details.session-options').open=true});
 await page.keyboard.press('Escape');assert.equal(await menu.getAttribute('open'),null);assert(await summary.evaluate(node=>node===document.activeElement));
 await summary.click();await menu.getByRole('button',{name:'Slow action',exact:true}).click();
 assert.equal(await menu.getAttribute('open'),null);await page.evaluate(()=>window.finishMenuAction());
 await summary.click();await menu.getByRole('button',{name:'Failing action',exact:true}).click();
 assert.equal(await menu.getAttribute('open'),null);assert.equal(await page.locator('#notice').innerText(),'Action failed');
 await summary.click();await page.getByLabel('Menu selection',{exact:true}).selectOption('b');
 assert.equal(await menu.getAttribute('open'),null);assert(await summary.evaluate(node=>node===document.activeElement));
 await page.getByRole('button',{name:'Inline one',exact:true}).click();
 await page.getByRole('button',{name:'Inline two',exact:true}).click();
 assert.equal(await page.locator('output').innerText(),'inline two');
 await page.getByRole('textbox',{name:'Outside menu'}).click();
 assert(await page.locator('.mode-verbose .source-menu').evaluate(node=>node.open));
 await page.evaluate(()=>document.querySelector('#verbose-dialog').showModal());
 await page.keyboard.press('Escape');
 assert.equal(await page.locator('#verbose-dialog').evaluate(node=>node.open),false);
 await summary.click();await menu.getByRole('button',{name:'Pin conversation',exact:true}).focus();await page.keyboard.press('Enter');
 assert(await summary.evaluate(node=>node===document.activeElement));
 await page.evaluate(()=>window.unmountDisclosures());checks.push({disclosureAction:true,escapeFocus:true,outsideFocus:true});
 await page.evaluate(()=>window.mountCleanup());
 const cleanup=page.getByRole('alert');await cleanup.waitFor();
 assert.match(await cleanup.innerText(),/Workspace files: Input is busy/);
 assert.doesNotMatch(await cleanup.innerText(),/opaque-session|panel:1:2|Error:/);
 assert.equal(await cleanup.locator('[data-session]').getAttribute('title'),'panel:1:2');
 await page.evaluate(()=>window.unmountCleanup());checks.push({humanCleanupLabel:true});
 await page.evaluate(()=>window.mountCleanupFlood());
 const flood=page.getByRole('alert',{name:'Resource cleanup'});await flood.waitFor();
 assert.equal(await flood.getByRole('button',{name:/^Retry cleanup for /}).count(),64);
 for(const width of [1440,390]){
  await page.setViewportSize({width,height:844});
  await flood.focus();await page.keyboard.press('Home');
  const geometry=await page.evaluate(()=>{const alert=document.querySelector('.resource-cleanup-notice'),main=document.querySelector('#workbench'),composer=document.querySelector('textarea');return {height:alert.clientHeight,scroll:alert.scrollHeight,main:main.clientHeight,bottom:composer.getBoundingClientRect().bottom,width:document.documentElement.scrollWidth}});
  assert(geometry.height<=844*.3+1&&geometry.scroll>geometry.height&&geometry.main>300&&geometry.bottom<=844&&geometry.width<=width,JSON.stringify(geometry));
  await page.screenshot({path:join(report,`cleanup-${width}.png`)});
  const last=flood.getByRole('button',{name:/^Retry cleanup for /}).last();await last.focus();
  const focused=await last.evaluate(node=>{const button=node.getBoundingClientRect(),alert=node.closest('[role=alert]').getBoundingClientRect();return {top:button.top,bottom:button.bottom,alertTop:alert.top,alertBottom:alert.bottom}});
  assert(focused.top>=focused.alertTop&&focused.bottom<=focused.alertBottom,JSON.stringify(focused));
  await page.screenshot({path:join(report,`cleanup-last-${width}.png`)});
  await page.keyboard.press('Enter');await page.waitForFunction(()=>window.cleanupCalls>64);
  checks.push({boundedCleanup:{width,...geometry}});
 }
 await page.evaluate(()=>window.unmountCleanupFlood());
 assert.deepEqual(errors,[]);await writeFile(join(report,'result.json'),JSON.stringify({checks,errors},null,2));
}catch(error){await writeFile(join(report,'failure.json'),JSON.stringify({error:String(error),errors,checks},null,2));throw error;}finally{await browser?.close();await server.close()}

}
