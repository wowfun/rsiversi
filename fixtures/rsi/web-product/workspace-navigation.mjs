// Real document/React tree with isolated transport: no model or filesystem credentials.
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {navigationOptions,navigationFilter,navigationMode} from './controls.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {setClipboardWriter} from './mounts.js';
setClipboardWriter(async text=>{
  if(fixture.rejectCopy)throw new Error('fixture clipboard unavailable');
  fixture.copied=text;
});
fixture.savedMain=fixture.view.surfaces.main;
fixture.view.surfaces.main=null;
fixture.view.setup.agent.default_model=null;
fixture.view.catalog.models=[];
const activeRow={session:'active',created_at_ms:'1',last_activity_ms:String(Date.now()-3600000),path:'/fixture',location:{kind:'local'},workspace:'fixture',metadata:{title:'Active conversation',archived:false,pinned:false}};
const archivedRow={...activeRow,session:'archived',metadata:{title:'Archived conversation',archived:true,pinned:false}};
fixture.view.navigation={can_archive:true,ticket:'nav',metadata_revision:'0',newer_activity:false,filter:{query:'',archived:false,workspace:{kind:'all'}},pins:[],groups:{},entries:[activeRow],more:false,continued:true,scanned:1,diagnostic:null};
fixture.queries=[];
fixture.setReply('command',async text=>{
 const command=JSON.parse(text);const nav=fixture.view.navigation;
 if(command.action==='navigate'&&command.command.kind==='query'){
   nav.filter=command.command.filter;
   nav.continued=false;
   fixture.queries.push(nav.filter.query);
   nav.entries=[activeRow,archivedRow].filter(row=>(nav.filter.archived===null||row.metadata.archived===nav.filter.archived)&&row.metadata.title.toLowerCase().includes(nav.filter.query.toLowerCase()));
   await fixture.frame();
 } else if(command.action==='mark_reading'){
   const {request}=command;
   const entry=nav.attention.entries.find(row=>row.position.conversation.id===request.position.conversation.id);
   if(request.action!=='acknowledge')entry.reading.manual=request.action==='unread';
   if(request.action!=='unread')entry.reading.unseen=false;
   fixture.lastMark=request;
   await fixture.frame();
 }
 return '{}';
});
fixture.archiveCalls=[];
fixture.setReply('archive_session',async text=>{
 const {session,action}=JSON.parse(text), nav=fixture.view.navigation;
 fixture.archiveCalls.push({session,action});
 if(session==='active' && action==='archive')return JSON.stringify([{kind:'job',items:[{id:'j1',label:'Compile preview'}]},{kind:'turn'}]);
 const row=session==='archived'?archivedRow:activeRow;
 row.metadata={...row.metadata,archived:action!=='restore',pinned:false};
 nav.entries=[activeRow,archivedRow].filter(row=>(nav.filter.archived===null||row.metadata.archived===nav.filter.archived)&&row.metadata.title.toLowerCase().includes(nav.filter.query.toLowerCase()));
 await fixture.frame();
 return 'null';
});
const createdDirectories=[];
fixture.setReply('directory_input',text=>{
 const request=JSON.parse(text);
 if(request.action==='status')return JSON.stringify({status:'ok',value:{supported:true,allowed:true}});
 if(request.action==='create'){
  const entry={name:request.name,path:request.parent+'/'+request.name,hidden:request.name.startsWith('.'),symlink:false};
  createdDirectories.push({parent:request.parent,entry});
  return JSON.stringify({status:'ok',value:{path:entry.path}});
 }
 if(request.action==='list'){
  if(request.path==='C:/delayed')return new Promise(resolve=>{
    fixture.delayedId=request.request_id;
    fixture.finishDelayed=()=>resolve(JSON.stringify({status:'ok',value:{path:'C:/delayed',breadcrumbs:['delayed'],entries:[]}}));
  });
  const path=request.path??'C:/projects';return JSON.stringify({status:'ok',value:{path,home:'C:/projects',breadcrumbs:path.slice(3).split('/').filter(Boolean),entries:path==='C:/projects'?[{name:'demo',path:'C:/projects/demo',hidden:false,symlink:false}]:createdDirectories.filter(item=>item.parent===path).map(item=>item.entry),truncated:false,unrepresentable:false}});
 }
 return '{}';
});
`;
const fixture=await sourceFixture({name:'workspace-navigation-fixture',source}),results=[];
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(fixture.url,{waitUntil:'commit'});
   await page.waitForFunction(()=>window.ready,null,{timeout:180_000});
   await page.evaluate(()=>fixture.frame());
   assert.equal(await page.locator('.external-navigation').count(),0);
   assert.equal(await page.getByRole('button',{name:'Toggle navigation',exact:true}).count(),0);
   assert.equal(await page.locator('#connection-state').isVisible(),false);
   assert.equal(await page.locator('.setup-prompt:visible,.pane-status:visible').count(),0);
   const searchButton=page.getByRole('button',{name:'Search conversations',exact:true});
   const searchInput=page.getByRole('textbox',{name:'Search conversations'});
   await searchButton.click();
   assert(await searchInput.evaluate(node=>node===document.activeElement));
   await page.locator('#workbench').click({position:{x:1,y:1}});
   await searchButton.waitFor();
   assert.deepEqual(await page.evaluate(()=>fixture.queries),[]);
   await searchButton.click();
   await searchInput.evaluate(input=>{
    // Both edits must arrive inside the debounce window even on a busy test host.
    const setValue=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;
    for(const text of ['zz','  Zz none  ']){
     setValue.call(input,text);
     input.dispatchEvent(new Event('input',{bubbles:true}));
    }
   });
   await page.getByText('No matching conversations.',{exact:true}).waitFor();
   assert.deepEqual(await page.evaluate(()=>fixture.queries),['Zz none']);
   await searchInput.fill('active');
   await searchInput.press('Enter');
   await page.getByTestId('conversation-row').filter({hasText:'Active conversation'}).waitFor();
   await searchInput.press('Escape');
   await page.waitForFunction(()=>fixture.queries.at(-1)==='');
   assert.deepEqual(await page.evaluate(()=>fixture.queries),['Zz none','active','']);
   await searchButton.click();
   await searchInput.fill('no active row has this name');
   await page.getByText('No matching conversations.',{exact:true}).waitFor();
   await searchInput.fill('ACTIVE');
   const matchingSearchRow=page.getByTestId('conversation-row').filter({hasText:'Active conversation'});
   await matchingSearchRow.waitFor();
   for(const term of ['conv ac','Áctive','active fixture']){
     await searchInput.fill(term);
     await matchingSearchRow.waitFor();
   }
   assert((await matchingSearchRow.locator('small').innerText()).trim().length>0);
   await page.screenshot({path:join(report,name+'-session-search.png')});
   await searchInput.press('Enter');
   await searchInput.fill('界'.repeat(499)+'🌿');
   assert.equal(await searchInput.inputValue(),'界'.repeat(499));
   await searchInput.press('Escape');
   assert.deepEqual(await page.evaluate(()=>fixture.queries),['Zz none','active','']);
   await navigationOptions(page);
   await page.getByRole('menuitemradio',{name:'By workspace',exact:true}).waitFor();
   for(const theme of ['dark','light']) {
    await page.emulateMedia({colorScheme:theme});
    await page.screenshot({path:join(report,name+'-'+theme+'-menu.png')});
   }
   await page.keyboard.press('Escape');
   assert(await page.locator('.navigation-options > summary').evaluate(node=>node===document.activeElement));
   await page.locator('.navigation-options > summary').press('ArrowDown');
   await page.keyboard.press('End');
   assert.equal(await page.getByRole('menuitem',{name:'Refresh',exact:true}).evaluate(node=>node===document.activeElement),true);
   await page.keyboard.press('Escape');
   await navigationMode(page,'flat');
   await navigationFilter(page,'all');
   await page.getByTestId('conversation-row').filter({hasText:'Archived conversation'}).waitFor();
   assert.equal(await page.getByTestId('conversation-row').count(),2);
   await navigationFilter(page,'archived');
   await page.getByTestId('conversation-row').filter({hasText:'Active conversation'}).waitFor({state:'detached'});
   assert.equal(await page.getByTestId('conversation-row').count(),1);
   await searchButton.click();await searchInput.fill('archived');
   const archivedResult=page.getByTestId('conversation-row').filter({hasText:'Archived conversation'});
   assert.equal(await archivedResult.getByTestId('conversation-open').getAttribute('aria-description'),'Unarchive this session to open it.');
   await archivedResult.getByTestId('conversation-open').click();
   const archiveWarning=page.getByRole('alert').filter({hasText:'Unarchive this session to open it.'});
   await archiveWarning.waitFor();
   assert(await archiveWarning.evaluate(node=>getComputedStyle(node).position==='fixed'));
   await archivedResult.getByRole('button',{name:'Unarchive session',exact:true}).click();
   await archivedResult.waitFor({state:'detached'});
   await page.getByText('No matching conversations.',{exact:true}).waitFor();
   await archiveWarning.waitFor({state:'detached'});
   await searchInput.press('Escape');
   await navigationFilter(page,'active');
   const sessionRow=page.getByTestId('conversation-row').filter({hasText:'Active conversation'});
   const sessionOpen=sessionRow.getByTestId('conversation-open');
   await page.setViewportSize({width:390,height:844});
   const opener=page.getByRole('button',{name:'Toggle navigation',exact:true});
   const drawer=page.getByRole('dialog',{name:'Workspace navigation',exact:true});
   await opener.click();await drawer.waitFor();
   await sessionOpen.focus();await sessionOpen.press('Shift+F10');
   const mobilePin=drawer.getByRole('menuitem',{name:'Pin session',exact:true});
   await mobilePin.waitFor();
   assert(await mobilePin.evaluate(node=>node===document.activeElement));
   await mobilePin.press('Escape');await mobilePin.waitFor({state:'detached'});
   assert(await drawer.isVisible());
   await sessionOpen.click();await drawer.waitFor({state:'hidden'});
   await opener.click();await drawer.getByRole('button',{name:'Open plugins',exact:true}).click();
   await drawer.waitFor({state:'hidden'});
   const pluginsPage=page.getByRole('region',{name:'Plugins',exact:true});
   await pluginsPage.waitFor();
   await pluginsPage.getByRole('button',{name:'Close plugins',exact:true}).click();await pluginsPage.waitFor({state:'hidden'});
   await page.setViewportSize({width:1440,height:900});
   await sessionOpen.waitFor();
   assert.equal(await sessionRow.getByRole('menuitem',{includeHidden:true}).count(),0);
   await sessionOpen.focus();
   await page.keyboard.press('Shift+F10');
   const pin=page.getByRole('menuitem',{name:'Pin session',exact:true});
   await pin.waitFor();
   await page.waitForFunction(()=>document.activeElement?.textContent==='Pin session');
   await page.keyboard.press('End');
   await page.waitForFunction(()=>document.activeElement?.textContent==='Archive session');
   await page.keyboard.press('Escape');
   await pin.waitFor({state:'detached'});
   assert(await sessionOpen.evaluate(node=>document.activeElement===node));
   await sessionOpen.click({button:'right'});
   await page.getByRole('menuitem',{name:'Archive session',exact:true}).click();
   const confirmArchive=page.getByRole('dialog',{name:'Stop and archive this session?',exact:true});
   await confirmArchive.getByText('1 background job: Compile preview').waitFor();
   assert(await confirmArchive.getByRole('button',{name:'Cancel'}).evaluate(node=>node===document.activeElement));
   await page.screenshot({path:join(report,name+'-archive-confirm.png')});
   await confirmArchive.getByRole('button',{name:'Cancel'}).click();
   assert.deepEqual(await page.evaluate(()=>fixture.archiveCalls.filter(row=>row.session==='active')),[{session:'active',action:'archive'}]);
   const stopAndArchive=async()=>{
     await sessionOpen.click({button:'right'});
     await page.getByRole('menuitem',{name:'Archive session',exact:true}).click();
     await confirmArchive.getByRole('button',{name:'Stop and archive',exact:true}).click();
     await confirmArchive.waitFor({state:'hidden'});
   };
   const archivedToast=page.getByRole('alert').filter({hasText:'Session stopped and archived.'});
   await stopAndArchive();
   const undoAction=archivedToast.getByRole('button',{name:'undo',exact:true});
   await undoAction.waitFor();
   assert.equal(await undoAction.evaluate(node=>getComputedStyle(node).color),'rgb(122, 170, 255)');
   assert.notEqual(await archivedToast.evaluate(node=>getComputedStyle(node).boxShadow),'none');
   await page.screenshot({path:join(report,name+'-archive-undo.png')});
   await archivedToast.getByRole('button',{name:'undo',exact:true}).click();
   await sessionOpen.waitFor();
   await stopAndArchive();
   await archivedToast.getByRole('button',{name:'filter archived sessions',exact:true}).click();
   await sessionOpen.waitFor();
   await sessionOpen.click({button:'right'});
   await page.getByRole('menuitem',{name:'Unarchive session',exact:true}).click();
   await page.waitForFunction(()=>fixture.archiveCalls.at(-1)?.action==='restore');
   await page.evaluate(async()=>{fixture.view.navigation.can_archive=false;await fixture.frame()});
   await sessionOpen.click({button:'right'});
   await page.getByRole('menuitem',{name:'Rename',exact:true}).waitFor();
   assert.equal(await page.getByRole('menuitem',{name:'Archive session',exact:true}).count(),0);
   await page.keyboard.press('Escape');
   await page.evaluate(async()=>{fixture.view.navigation.can_archive=true;await fixture.frame()});
   await sessionOpen.click({button:'right'});
   await page.getByRole('menuitem',{name:'Rename',exact:true}).click();
   const rename=page.getByRole('dialog',{name:'Rename session',exact:true});
   await rename.waitFor();
   assert(await rename.getByLabel('Session name').evaluate(node=>document.activeElement===node));
   assert(await rename.evaluate(node=>!getComputedStyle(node).backgroundColor.startsWith('rgba(')),'the rename dialog must have an opaque background');
   const nameField=rename.getByLabel('Session name');
   const closeRename=rename.getByRole('button',{name:'Close rename session'}),saveRename=rename.getByRole('button',{name:'Rename',exact:true});
   await closeRename.focus();await page.keyboard.press('Shift+Tab');
   assert(await saveRename.evaluate(el=>el===document.activeElement),'backward Tab wraps inside the rename dialog');
   await page.keyboard.press('Tab');
   assert(await closeRename.evaluate(el=>el===document.activeElement),'forward Tab wraps inside the rename dialog');
   await nameField.dispatchEvent('compositionstart');
   await nameField.press('Enter');
   assert(await rename.isVisible(),'committing composition must keep the dialog open');
   await nameField.dispatchEvent('compositionend');
   await page.screenshot({path:join(report,name+'-rename-session.png')});
   await page.keyboard.press('Escape');
   await rename.waitFor({state:'hidden'});
   assert(await sessionOpen.evaluate(node=>document.activeElement===node));
   await sessionOpen.press('ContextMenu');
   await pin.waitFor();
   assert(await pin.evaluate(node=>[...node.querySelectorAll('use')].every(icon=>document.querySelector(icon.getAttribute('href')))));
   await page.screenshot({path:join(report,name+'-session-actions.png')});
   await page.keyboard.press('Escape');
   const actions=sessionRow.getByRole('button',{name:/Session actions for/});
   await actions.click();await page.getByRole('menuitem',{name:'Copy Session ID',exact:true}).click();
   const copied=page.getByRole('alert').filter({hasText:'Session ID copied'});
   await copied.waitFor();
   assert.equal(await page.evaluate(()=>fixture.copied),'active');
   await page.evaluate(()=>{fixture.rejectCopy=true});
   await actions.click();await page.getByRole('menuitem',{name:'Copy Session ID',exact:true}).click();
   const copyFailed=page.getByRole('alert').filter({hasText:'Copy failed. Try again.'});
   await copyFailed.waitFor();assert.equal(await copied.count(),0);
   assert(await copyFailed.locator('svg use').evaluate(use=>!!document.querySelector(use.getAttribute('href'))));
   await page.waitForFunction(()=>[...document.querySelectorAll('[role="alert"]')].some(el=>el.textContent==='Copy failed. Try again.' && Number(getComputedStyle(el).opacity)>.98));
   await page.screenshot({path:join(report,name+'-copy-warning.png')});
   await copyFailed.waitFor({state:'detached'});
   await actions.click();await pin.waitFor();
   await actions.click();await pin.waitFor({state:'detached'});
   await page.evaluate(async()=>{
     fixture.view.navigation.attention={host_epoch:'00000000000000000000000000000001',entries:[{position:{conversation:{kind:'native',id:'active'},epoch:'0',sequence:'1'},reading:{manual:false,unseen:false,failed:false},status:'running',targets:[]}],truncated:false};
     await fixture.frame();
   });
   const indicator=sessionRow.getByRole('img',{name:'Running'});
   await indicator.waitFor();
   await actions.click();
   await page.getByRole('menuitem',{name:/^Mark as unread/}).click();
   await page.waitForFunction(()=>fixture.lastMark?.action==='unread');
   const mark=await page.evaluate(()=>fixture.lastMark);
   assert.deepEqual(mark,{host_epoch:'00000000000000000000000000000001',action:'unread',position:{conversation:{kind:'native',id:'active'},epoch:'0',sequence:'1'}});
   assert(await indicator.isVisible(),'live work still takes priority over the manual reminder');
   await actions.click();
   await page.getByRole('menuitem',{name:/^Mark as read/}).click();
   await page.waitForFunction(()=>fixture.lastMark?.action==='read');
   assert(await sessionRow.evaluate(node=>node.querySelector('.session-activity').getBoundingClientRect().right<=node.querySelector('strong').getBoundingClientRect().left),'flat row activity must clear the title');

   await page.getByRole('button',{name:'Choose workspace',exact:true}).click();
   const dialog=page.getByRole('dialog',{name:'Select workspace directory',exact:true});
   const location=dialog.getByLabel('Execution location',{exact:true});
   const reads=()=>page.evaluate(()=>fixture.calls.filter(call=>{
     if(call.method!=='command')return false;
     const input=JSON.parse(call.payload);
     return input.action==='plugins'&&input.command.kind==='ssh'&&input.command.command.kind==='read';
   }).length);
   await dialog.getByRole('button',{name:'demo',exact:false}).waitFor();
   assert.equal(await reads(),0);
   assert.equal(await location.count(),0);
   await page.evaluate(async()=>{
     fixture.previousPlugins=fixture.view.plugins;
     fixture.view.plugins={...fixture.view.plugins,ssh:{available:true,catalog:{targets:[
       {candidate:{target:'remote',name:'Remote fixture'},permissions:{use_target:true},connected:true},
       {candidate:{target:'hidden',name:'Unavailable fixture'},permissions:{use_target:false},connected:true},
     ]}}};
     await fixture.frame();
   });
   await location.selectOption('remote');
   assert.equal(await reads(),1);
   assert.equal(await location.getByRole('option',{name:'Unavailable fixture'}).count(),0);
   await page.evaluate(async()=>{fixture.view.plugins=fixture.previousPlugins;await fixture.frame()});
   await location.getByRole('option',{name:'Selected SSH target · unavailable'}).waitFor({state:'attached'});
   await location.selectOption('local');
   await location.waitFor({state:'detached'});assert.equal(await reads(),1);
   await dialog.getByRole('button',{name:'demo',exact:false}).click();
   const breadcrumbs=dialog.getByRole('navigation',{name:'Directory breadcrumbs'});
   await breadcrumbs.getByRole('button',{name:'demo',exact:true}).waitFor();
   assert.deepEqual(await breadcrumbs.getByRole('button').allTextContents(),['Home','demo']);
   assert.equal(await breadcrumbs.locator('svg use[href="#dsw-icon-chevron-right-outline-regular"]').count(),1);
   const editZone=dialog.getByRole('button',{name:'Edit directory path',exact:true});
   assert((await editZone.boundingBox()).width>100,'unused space after a short path opens the editor');
   const selected=dialog.locator('button[aria-current="true"]');
   assert(await selected.evaluate(node=>node===document.activeElement));
   const newFolder=dialog.getByRole('button',{name:'New folder',exact:true});
   const prompt=page.getByRole('dialog',{name:'New folder',exact:true});
   const folderName=prompt.getByRole('textbox',{name:'Folder name'});
   await newFolder.click();await folderName.fill('cancelled');
   await folderName.press('Escape');await prompt.waitFor({state:'detached'});
   assert(await newFolder.evaluate(node=>node===document.activeElement));
   await newFolder.press('Enter');
   assert.equal(await folderName.inputValue(),'');
   await folderName.fill('   ');
   assert(await prompt.getByRole('button',{name:'Create',exact:true}).isDisabled());
   await folderName.fill('.picked-on-target');
   await page.screenshot({path:join(report,name+'-new-folder.png')});
   await folderName.press('Enter');
   await dialog.getByRole('navigation',{name:'Directory breadcrumbs',exact:true}).getByRole('button',{name:'.picked-on-target',exact:true}).waitFor();
   assert.equal(await dialog.getByRole('button',{name:'.picked-on-target',exact:true}).count(),2);
   assert.equal(await selected.textContent(),'.picked-on-target');
   assert(await selected.evaluate(node=>[...node.querySelectorAll('use')].every(icon=>document.querySelector(icon.getAttribute('href')))));
   assert(await selected.evaluate(node=>node===document.activeElement)||
     await newFolder.evaluate(node=>node===document.activeElement),'keyboard focus must remain on the child or its surviving creation trigger');
   assert.equal(await page.evaluate(()=>fixture.calls.filter(call=>call.method==='directory_input'&&JSON.parse(call.payload).action==='create').length),1);
   await dialog.locator('footer').getByRole('button',{name:'Open',exact:true}).click();
   await dialog.waitFor({state:'detached'});
   const registrations=await page.evaluate(()=>fixture.calls.filter(call=>call.method==='command').map(call=>JSON.parse(call.payload)).filter(command=>command.action==='register_workspace'));
   assert.deepEqual(registrations,[{action:'register_workspace',location:{kind:'local'},path:'C:/projects/demo/.picked-on-target'}]);
   const cancels=()=>page.evaluate(()=>fixture.calls.filter(call=>call.method==='directory_input').map(call=>JSON.parse(call.payload)).filter(request=>request.action==='cancel').map(request=>request.request_id));
   assert.deepEqual(await cancels(),[]);
   await page.getByRole('button',{name:'Choose workspace',exact:true}).click();
   const editPath=dialog.getByRole('button',{name:'Edit directory path',exact:true});
   await editPath.click();
   const pathInput=dialog.getByRole('textbox',{name:'Directory path',exact:true});
   await pathInput.fill('C:/elsewhere');await pathInput.press('Enter');
   await dialog.getByRole('navigation',{name:'Directory breadcrumbs'}).getByRole('button',{name:'elsewhere',exact:true}).waitFor();
   assert.deepEqual(await breadcrumbs.getByRole('button').allTextContents(),['C:/','elsewhere']);
   const parts=Array.from({length:16},(_,i)=>`directory-segment-${i}`),longPath='C:/'+parts.join('/');
   await editPath.click();await pathInput.fill(longPath);await pathInput.press('Enter');
   await breadcrumbs.getByRole('button',{name:parts.at(-1),exact:true}).waitFor();
   const endVisible=()=>breadcrumbs.evaluate(trail=>trail.scrollLeft>0&&trail.scrollWidth-trail.clientWidth-trail.scrollLeft<2);
   assert(await endVisible(),'long paths reveal their last segment');
   await editPath.click();await pathInput.press('Escape');
   assert(await endVisible(),'returning from path entry restores the selected end');
   await editPath.click();
   await pathInput.fill('C:/delayed');await pathInput.press('Enter');
   await page.waitForFunction(()=>!!fixture.delayedId);
   await pathInput.fill('C:/newer');
   await page.evaluate(()=>fixture.finishDelayed());
   await page.waitForFunction(()=>document.querySelector('.directory-columns').getAttribute('aria-busy')==='false');
   assert.equal(await pathInput.inputValue(),'C:/newer');
   assert(await newFolder.isDisabled());
   await pathInput.press('Escape');await editPath.waitFor();
   assert(await editPath.evaluate(node=>node===document.activeElement));
   await page.keyboard.press('Escape');await dialog.waitFor({state:'detached'});
   const delayedId=await page.evaluate(()=>fixture.delayedId);
   assert.deepEqual(await cancels(),[delayedId]);
   await page.evaluate(()=>{
     fixture.setReply('directory_input',text=>{
       const input=JSON.parse(text);
       if(input.action==='status')return JSON.stringify({status:'ok',value:{supported:true,allowed:true}});
       if(input.action==='list')return new Promise(resolve=>{fixture.pendingRead=input.request_id;fixture.finishRead=resolve});
       if(input.action==='cancel'){fixture.finishRead('{}');return 'null'}
     });
   });
   await page.getByRole('button',{name:'Choose workspace',exact:true}).click();
   await page.waitForFunction(()=>!!fixture.pendingRead);
   await page.keyboard.press('Escape');await dialog.waitFor({state:'detached'});
   assert.deepEqual(await cancels(),[delayedId,await page.evaluate(()=>fixture.pendingRead)]);
   assert.equal(await page.getByRole('button',{name:'Toggle resources',exact:true}).count(),0);
   assert.equal(await page.locator('aside.resources').isVisible(),false);
   await page.screenshot({path:join(report,name+'-starting.png')});
   await navigationMode(page,'workspace');
   await page.evaluate(async()=>{
     const entries=Array.from({length:9},(_,i)=>({session:'row-'+i,created_at_ms:'1',last_activity_ms:String(Date.now()-60000*(i+1)),location:{kind:'local'},path:'/fixture',workspace:'fixture',metadata:{title:'Conversation '+i,archived:false,pinned:false}}));
     fixture.view.navigation.groups.fixture={ticket:'group',workspace:{kind:'registered',id:'fixture'},entries,more:false,continued:false,stale:false,scanned:9};
     await fixture.frame();
   });
   const group=page.getByTestId('workspace-group').filter({has:page.getByText('Conversation 0',{exact:true})});
   await group.getByTestId('conversation-row').first().waitFor();
   const ungrouped=page.getByRole('button',{name:'Ungrouped',exact:true});
   await ungrouped.waitFor({state:'detached'});
   await page.evaluate(async()=>{fixture.view.navigation.more=true;await fixture.frame()});
   await ungrouped.waitFor();
   await page.evaluate(async()=>{
     const nav=fixture.view.navigation;
     nav.more=false;
     nav.entries.push({...nav.groups.fixture.entries[0],session:'loose',workspace:null});
     await fixture.frame();
   });
   await ungrouped.waitFor();
   await page.evaluate(async()=>{fixture.view.navigation.entries.pop();await fixture.frame()});
   await ungrouped.waitFor({state:'detached'});
   assert.equal(await group.getByTestId('conversation-row').count(),5);
   const more=group.getByRole('button',{name:'Show 4 more sessions',exact:true});
   assert.equal(await more.getAttribute('aria-expanded'),'false');
   await more.click();
   assert.equal(await group.getByTestId('conversation-row').count(),9);
   assert.equal(await group.getByRole('button',{name:'Show less',exact:true}).getAttribute('aria-expanded'),'true');
   await group.getByRole('button',{name:'Show less',exact:true}).click();
   assert.equal(await group.getByTestId('conversation-row').count(),5);
   await page.evaluate(async()=>{window.allRows=fixture.view.navigation.groups.fixture.entries;fixture.view.navigation.groups.fixture.entries=window.allRows.slice(0,6);await fixture.frame()});
   await group.getByRole('button',{name:'Show 1 more session',exact:true}).waitFor();
   await page.evaluate(async()=>{
    fixture.view.navigation.groups.fixture.entries=window.allRows;
    fixture.view.surfaces.main={...fixture.savedMain,session:'row-7',draft:false,workspace:fixture.attachedWorkspace('fixture')};
    fixture.view.navigation.attention={host_epoch:'00000000000000000000000000000001',entries:[{position:{conversation:{kind:'native',id:'row-7'},epoch:'0',sequence:'1'},reading:{manual:false,unseen:false,failed:false},status:'running',targets:[]}],truncated:false};
    await fixture.frame();
   });
   await group.locator('[aria-current=true]').waitFor();
   const activeEditor=page.getByRole('textbox',{name:'Main message'});
   const options=page.locator('details.navigation-options');
   await options.locator('summary').click();
   await options.getByRole('menu').press('Control+Shift+u');
   assert.notEqual(await page.evaluate(()=>fixture.lastMark?.position.conversation.id),'row-7');
   await options.locator('summary').click();
   await activeEditor.press('Control+Shift+u');
   await page.waitForFunction(()=>fixture.lastMark?.position.conversation.id==='row-7' && fixture.lastMark.action==='unread');
   await activeEditor.press('Control+Shift+u');
   await page.waitForFunction(()=>fixture.lastMark?.position.conversation.id==='row-7' && fixture.lastMark.action==='read');
   await page.evaluate(async()=>{
     const row=fixture.view.navigation.attention.entries[0];
     row.position.sequence='15';row.reading.unseen=true;
     fixture.view.surfaces.main.transcript.turns={entries:{next:{id:'next',status:'Running',running:true,foldable:false,partial:false,blocks:[],process:[],candidate:[],answer:[],reading_sequence:'9'}}};
     await fixture.frame();
     await new Promise(done=>requestAnimationFrame(()=>requestAnimationFrame(done)));
   });
   assert.equal(await page.evaluate(()=>fixture.lastMark.action),'read','an earlier boundary must leave a later completion unread');
   await page.evaluate(async()=>{
     fixture.view.surfaces.main.transcript.turns.entries.next.reading_sequence='100';
     await fixture.frame();
   });
   await page.waitForFunction(()=>fixture.lastMark.action==='acknowledge' && fixture.lastMark.position.sequence==='15');
   await group.getByRole('button',{name:'Show 1 more session',exact:true}).click();
   const opens=()=>page.evaluate(()=>fixture.calls.filter(call=>call.method==='command' && JSON.parse(call.payload).action==='open').length);
   const priorOpens=await opens();
   await page.getByRole('button',{name:'Open plugins',exact:true}).click();
   await page.getByRole('region',{name:'Plugins',exact:true}).waitFor();
   await page.evaluate(async()=>{
     const entry=fixture.view.navigation.attention.entries[0];
     entry.status='unread';entry.reading.manual=true;
     await fixture.frame();
   });
   const attached=group.locator('[data-session-id="row-7"]');
   await attached.getByRole('img',{name:'Unread'}).waitFor();
   await attached.getByTestId('conversation-open').click();
   await activeEditor.waitFor();
   assert.equal(await opens(),priorOpens,'returning from a global page must preserve the attached surface');
   await page.waitForFunction(()=>fixture.lastMark.action==='read' && fixture.lastMark.position.sequence==='15');
   await group.getByRole('button',{name:'Show less',exact:true}).click();
   assert.equal(await group.getByTestId('conversation-row').count(),8);
   const alignment=await group.evaluate(node=>{
    const workspace=node.querySelector('.workspace-toggle strong').getBoundingClientRect(),row=node.querySelector('[aria-current=true]'),text=row.querySelector('strong').getBoundingClientRect();
    return {aligned:Math.abs(workspace.left-text.left)<2,compact:row.getBoundingClientRect().height<=36,active:!!node.querySelector('.workspace-row[data-active]')};
   });
   assert.deepEqual(alignment,{aligned:true,compact:true,active:true});
   await page.screenshot({path:join(report,name+'-workspace-rows.png')});
   await page.evaluate(async()=>{
     fixture.view.surfaces.main={...fixture.savedMain,session:'fresh-session',draft:true,creation:{workspace_id:'fixture'}};
     await fixture.frame();
   });
   const fresh=group.getByRole('button',{name:'New session',exact:true});
   await fresh.waitFor();
   assert.equal(await fresh.getAttribute('aria-current'),'true');
   const editor=page.getByRole('textbox',{name:'Main message'});
   await editor.fill('Still editing');
   await fresh.press('Enter');
   await page.waitForFunction(()=>document.activeElement?.getAttribute('aria-label')==='Main message');
   assert.equal(await editor.inputValue(),'Still editing');
   await page.evaluate(async()=>{
     const nav=fixture.view.navigation;
     const sample=nav.groups.fixture.entries[0];
     const blanks=['other-queued','fresh-session'].map(session=>({...sample,session,blank:true,metadata:{...sample.metadata,title:session}}));
     nav.entries.push(...blanks);
     nav.groups.fixture.entries.push(...blanks);
     await fixture.frame();
   });
   const persisted=group.getByTestId('conversation-row').first();
   await persisted.getByText('New session',{exact:true}).waitFor();
   assert.equal(await persisted.getAttribute('data-session-id'),'fresh-session');
   assert.equal(await group.getByTestId('conversation-row').count(),9);
   assert.equal(await persisted.evaluate(node=>node.draggable),false);
   assert.equal(await persisted.locator('time,[data-session-menu-trigger]').count(),0);
   await persisted.getByTestId('conversation-open').press('Shift+F10');
   assert.equal(await page.getByRole('menu').filter({has:page.getByRole('menuitem',{name:'Pin session'})}).count(),0);
   assert.equal(await group.locator('[data-session-id="other-queued"]').count(),0);
   await persisted.getByTestId('conversation-open').click();
   assert.equal(await opens(),priorOpens);
   assert.equal(await editor.inputValue(),'Still editing');
   await page.screenshot({path:join(report,name+'-selected-blank.png'),animations:'disabled'});
   await page.evaluate(async()=>{
     const nav=fixture.view.navigation;
     const blank=nav.entries.find(entry=>entry.session==='fresh-session');
     nav.pins=[{status:'available',entry:{...blank,metadata:{...blank.metadata,pinned:true}}}];
     nav.entries=nav.entries.filter(entry=>entry.session!==blank.session);
     nav.groups.fixture.entries=nav.groups.fixture.entries.filter(entry=>entry.session!==blank.session);
     nav.attention.entries=[];
     window.foldedPage={...nav.groups.fixture,entries:Array.from({length:16},(_,i)=>({...window.allRows[0],session:'sample-'+i,metadata:{...window.allRows[0].metadata,title:'Conversation '+i}}))};
     delete nav.groups.fixture;
     await fixture.frame();
   });
   await page.getByTestId('workspace-group').locator('[data-session-id="fresh-session"]').waitFor({state:'detached'});
   await page.evaluate(async()=>{fixture.view.navigation.groups.fixture=window.foldedPage;await fixture.frame()});
   await group.getByRole('button',{name:'Show 11 more sessions',exact:true}).waitFor();
   assert.equal(await group.getByTestId('conversation-row').count(),6);
   assert.equal(await page.locator('.session-row[data-session-id="fresh-session"]').count(),1);
   for(const [hidden,count] of [[11,11],[6,16],[1,17]]){
     await group.getByRole('button',{name:`Show ${hidden} more ${hidden===1?'session':'sessions'}`,exact:true}).click();
     assert.equal(await group.getByTestId('conversation-row').count(),count);
   }
   await group.getByRole('button',{name:'Show less',exact:true}).click();
   assert.equal(await group.getByTestId('conversation-row').count(),6);
   await page.screenshot({path:join(report,name+'-folded-workspace.png'),animations:'disabled'});
   await page.evaluate(async()=>{
     const nav=fixture.view.navigation;
     const blank=nav.pins[0].entry;
     nav.pins=[];
     nav.entries.push(blank);
     nav.groups.fixture.entries=[...window.allRows,blank];
     await fixture.frame();
   });
   await searchButton.click();
   for(const id of ['fresh-session','other-queued']){
     await searchInput.fill(id);
     await page.getByText('No matching conversations.',{exact:true}).waitFor();
     assert.equal(await page.locator('#sessions .session-row').count(),0);
   }
   await searchInput.press('Escape');
   await page.evaluate(async()=>{
     const workspace={...fixture.savedMain.workspace,registration:{status:'unregistered'}};
     fixture.view.surfaces.main={...fixture.savedMain,session:'loose-draft',draft:true,workspace};
     const sample=fixture.view.navigation.entries[0];
     fixture.view.navigation.entries.push({...sample,session:'loose-draft',blank:true,workspace:null});
     await fixture.frame();
   });
   await ungrouped.waitFor();
   await page.evaluate(async()=>{
     fixture.view.surfaces.main={...fixture.savedMain,session:'row-7',draft:false};
     await fixture.frame();
   });
   await fresh.waitFor({state:'detached'});
   await ungrouped.waitFor({state:'detached'});
   await page.evaluate(()=>{
     window.groupOpens=0;
     fixture.setReply('command',async text=>{
       const request=JSON.parse(text).command;
       if(request?.kind==='close_group'){
         delete fixture.view.navigation.groups.fixture;
         await fixture.frame();
         await new Promise(resolve=>{window.finishClose=resolve;});
       }
       if(request?.kind==='group')window.groupOpens++;
       return '{}';
     });
   });
   const toggle=group.locator('.workspace-toggle');
   await toggle.click();
   await page.waitForFunction(()=>!!window.finishClose);
   await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
   await page.evaluate(()=>window.finishClose());
   await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
   assert.equal(await page.evaluate(()=>window.groupOpens),0,'closing the active workspace must not enqueue an automatic reopen');
   assert.deepEqual(errors,[]);results.push({browser:name,menu:true,keyboard:true,sessionContextMenu:true,composerReadShortcut:true,
     acknowledgedOnlyAfterLaterTurn:true,archiveAndCopyFeedbackExpires:true,sessionDialogFocus:true,flatActivityClearsTitle:true,collapseSurvivesEarlyFrame:true,archiveFilters:true,debouncedSearch:true,completePageSearchWithoutRpc:true,modelFreeRegistration:true,resourceFallback:true});
  } catch (error) {
   const failure=join(report,name+'-failure.txt');
   await writeFile(failure,[error.stack,...errors].join('\n'));
   await page.screenshot({path:join(report,name+'-failure.png'),timeout:5000}).catch(()=>{});
   const body=await page.locator('body').innerText({timeout:5000}).catch(()=>'');
   if(body)await writeFile(failure,[error.stack,...errors,body].join('\n'));
   throw error;
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await fixture.close()}
