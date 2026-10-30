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
fixture.view.navigation={ticket:'nav',metadata_revision:'0',newer_activity:false,filter:{query:'',archived:false,workspace:{kind:'all'}},pins:[],groups:{},entries:[activeRow],more:false,scanned:1,diagnostic:null};
fixture.setReply('command',async text=>{
 const command=JSON.parse(text);const nav=fixture.view.navigation;
 if(command.action==='navigate'&&command.command.kind==='query'){
   nav.filter=command.command.filter;nav.entries=[activeRow,archivedRow].filter(row=>nav.filter.archived===null||row.metadata.archived===nav.filter.archived);await fixture.frame();
 }
 return '{}';
});
fixture.setReply('directory_input',text=>{
 const request=JSON.parse(text);
 if(request.action==='status')return JSON.stringify({status:'ok',value:{supported:true,allowed:true}});
 if(request.action==='list'){
  const path=request.path??'C:/projects';return JSON.stringify({status:'ok',value:{path,home:'C:/projects',breadcrumbs:path.slice(3).split('/').filter(Boolean),entries:path==='C:/projects'?[{name:'demo',path:'C:/projects/demo',hidden:false,symlink:false}]:[],truncated:false,unrepresentable:false}});
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
   await navigationFilter(page,'active');
   await page.getByRole('button',{name:'Choose workspace',exact:true}).click();
   const dialog=page.getByRole('dialog',{name:'Select workspace directory',exact:true});
   await dialog.getByRole('button',{name:'demo',exact:false}).click();
   await dialog.getByRole('navigation',{name:'Directory breadcrumbs'}).getByRole('button',{name:'C:/',exact:true}).waitFor();
   await dialog.locator('footer').getByRole('button',{name:'Open',exact:true}).click();
   await dialog.waitFor({state:'detached'});
   const registrations=await page.evaluate(()=>fixture.calls.filter(call=>call.method==='command').map(call=>JSON.parse(call.payload)).filter(command=>command.action==='register_workspace'));
   assert.deepEqual(registrations,[{action:'register_workspace',location:{kind:'local'},path:'C:/projects/demo'}]);
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
    fixture.view.surfaces.main={...fixture.savedMain,session:'row-7',workspace:'fixture'};
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
   assert.deepEqual(errors,[]);results.push({browser:name,menu:true,keyboard:true,archiveFilters:true,modelFreeRegistration:true,resourceFallback:true});
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await fixture.close()}
