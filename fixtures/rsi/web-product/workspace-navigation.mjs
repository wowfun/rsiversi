// Real document/React tree with isolated transport: no model or filesystem credentials.
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {navigationOptions,navigationFilter,navigationMode} from './controls.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.savedMain=fixture.view.surfaces.main;
fixture.view.surfaces.main=null;
fixture.view.setup.agent.default_model=null;
fixture.view.catalog.models=[];
const activeRow={session:'active',created_at_ms:'1',last_activity_ms:String(Date.now()-3600000),path:'/fixture',location:{kind:'local'},workspace:'fixture',metadata:{title:'Active conversation',archived:false,pinned:false}};
const archivedRow={...activeRow,session:'archived',metadata:{title:'Archived conversation',archived:true,pinned:false}};
fixture.view.navigation={ticket:'nav',metadata_revision:'0',newer_activity:false,filter:{query:'',archived:false,workspace:{kind:'all'}},pins:[],groups:{},entries:[activeRow],more:false,continued:true,scanned:1,diagnostic:null};
fixture.queries=[];
fixture.setReply('command',async text=>{
 const command=JSON.parse(text);const nav=fixture.view.navigation;
 if(command.action==='navigate'&&command.command.kind==='query'){
   nav.filter=command.command.filter;
   nav.continued=false;
   fixture.queries.push(nav.filter.query);
   nav.entries=[activeRow,archivedRow].filter(row=>(nav.filter.archived===null||row.metadata.archived===nav.filter.archived)&&row.metadata.title.toLowerCase().includes(nav.filter.query.toLowerCase()));
   await fixture.frame();
 } else if(command.action==='navigate'&&command.command.kind==='replace'&&command.command.session==='archived'){
   archivedRow.metadata=command.command.metadata;
   nav.entries=nav.entries.filter(row=>nav.filter.archived===null||row.metadata.archived===nav.filter.archived);
   await fixture.frame();
 }
 return '{}';
});
const createdDirectories=[];
fixture.setReply('directory_input',text=>{
 const request=JSON.parse(text);
 if(request.action==='status')return JSON.stringify({status:'ok',value:{supported:true,allowed:true}});
 if(request.action==='create'){
  const entry={name:request.name,path:request.parent+'/'+request.name,hidden:false,symlink:false};
  createdDirectories.push({parent:request.parent,entry});
  return JSON.stringify({status:'ok',value:{path:entry.path}});
 }
 if(request.action==='list'){
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
   await page.goto(fixture.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
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
   await archivedResult.getByTestId('conversation-open').click();
   await page.getByText('Unarchive this session to open it.',{exact:true}).waitFor();
   await archivedResult.getByRole('button',{name:'Unarchive session',exact:true}).click();
   await archivedResult.waitFor({state:'detached'});
   await page.getByText('No matching conversations.',{exact:true}).waitFor();
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
   const pluginSettings=page.getByRole('dialog',{name:'Settings',exact:true});
   await pluginSettings.waitFor();await page.keyboard.press('Escape');await pluginSettings.waitFor({state:'hidden'});
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
   await actions.click();await pin.waitFor();
   await actions.click();await pin.waitFor({state:'detached'});
   await page.evaluate(async()=>{
     fixture.view.navigation.attention={entries:[{position:{conversation:{kind:'native',id:'active'},epoch:'fixture',sequence:'1'},status:'running',targets:[]}],truncated:false};
     await fixture.frame();
   });
   const indicator=sessionRow.getByRole('img',{name:'Running'});
   await indicator.waitFor();
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
   await dialog.getByRole('navigation',{name:'Directory breadcrumbs'}).getByRole('button',{name:'C:/',exact:true}).waitFor();
   await dialog.getByRole('button',{name:'+ New folder',exact:true}).click();
   await dialog.getByLabel('Folder name',{exact:true}).fill('picked-on-target');
   await dialog.getByRole('button',{name:'Create folder',exact:true}).click();
   await dialog.getByRole('navigation',{name:'Directory breadcrumbs',exact:true}).getByRole('button',{name:'picked-on-target',exact:true}).waitFor();
   assert.equal(await dialog.getByRole('button',{name:'picked-on-target',exact:true}).count(),2);
   assert.equal(await page.evaluate(()=>fixture.calls.filter(call=>call.method==='directory_input'&&JSON.parse(call.payload).action==='create').length),1);
   await dialog.locator('footer').getByRole('button',{name:'Open',exact:true}).click();
   await dialog.waitFor({state:'detached'});
   const registrations=await page.evaluate(()=>fixture.calls.filter(call=>call.method==='command').map(call=>JSON.parse(call.payload)).filter(command=>command.action==='register_workspace'));
   assert.deepEqual(registrations,[{action:'register_workspace',location:{kind:'local'},path:'C:/projects/demo/picked-on-target'}]);
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
    fixture.view.surfaces.main={...fixture.savedMain,session:'row-7',workspace:fixture.attachedWorkspace('fixture')};
    fixture.view.navigation.attention={entries:[{position:{conversation:{kind:'native',id:'row-7'},epoch:'fixture',sequence:'1'},status:'running',targets:[]}],truncated:false};
    await fixture.frame();
   });
   await group.locator('[aria-current=true]').waitFor();
   assert.equal(await group.getByTestId('conversation-row').count(),6);
   const alignment=await group.evaluate(node=>{
    const workspace=node.querySelector('.workspace-toggle strong').getBoundingClientRect(),row=node.querySelector('[aria-current=true]'),text=row.querySelector('strong').getBoundingClientRect();
    return {aligned:Math.abs(workspace.left-text.left)<2,compact:row.getBoundingClientRect().height<=36,active:!!node.querySelector('.workspace-row[data-active]')};
   });
   assert.deepEqual(alignment,{aligned:true,compact:true,active:true});
   await page.screenshot({path:join(report,name+'-workspace-rows.png')});
   await page.evaluate(async()=>{
     fixture.view.surfaces.main={...fixture.savedMain,session:'fresh-session',draft:true};
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
     const workspace={...fixture.savedMain.workspace,registration:{status:'unregistered'}};
     fixture.view.surfaces.main={...fixture.savedMain,session:'loose-draft',draft:true,workspace};
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
   assert.deepEqual(errors,[]);results.push({browser:name,menu:true,keyboard:true,sessionContextMenu:true,sessionDialogFocus:true,flatActivityClearsTitle:true,collapseSurvivesEarlyFrame:true,archiveFilters:true,debouncedSearch:true,completePageSearchWithoutRpc:true,modelFreeRegistration:true,resourceFallback:true});
  } catch (error) {
   await page.screenshot({path:join(report,name+'-failure.png')});
   await writeFile(join(report,name+'-failure.txt'),[error.stack,...errors,await page.locator('body').innerText()].join('\n'));
   throw error;
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await fixture.close()}
