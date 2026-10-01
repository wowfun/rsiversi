// Product path: authenticated browser, Rust Worker, Host, Store and 65 durable Sessions.
import {paired} from './paired-env.mjs';
import {startService} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
const report=process.env.RSI_REPORT_DIR;
assert(report,'RSI_REPORT_DIR must name a new isolated evidence directory');
await mkdir(report,{recursive:true});
let service,browser,page;const errors=[],sessions=[],checks=[];
try{
  service=await startService({binary:paired.binary,assets:paired.assets,report});
  browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900},deviceScaleFactor:1});
  await context.addInitScript(()=>{
    const NativeWorker=window.Worker;window.bridgeErrors=[];
    window.Worker=class extends NativeWorker {
      constructor(...args){super(...args);this.calls=new Map();this.addEventListener('message',({data})=>{
        if(data.kind!=='reply')return;const request=this.calls.get(data.id);this.calls.delete(data.id);
        if(data.error && window.bridgeErrors.length<100)window.bridgeErrors.push({...request,error:data.error});
      });}
      postMessage(data,...rest){if(data.kind==='call'){let input;try{if(typeof data.payload==='string'&&data.payload.length<65536)input=JSON.parse(data.payload);}catch{}this.calls.set(data.id,{method:data.method,action:input?.action,generation:input?.generation});}super.postMessage(data,...rest);}
    };
  });
  page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
  await connectWorkbench(page,service,'Indexed navigation');await openWorkspace(page,service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});
  for(let index=0;index<65;index++){
    if(index)await page.getByTestId('workspace-open').first().click();
    await pane.getByLabel('Main message',{exact:true}).fill(`Navigation member ${String(index).padStart(3,'0')}`);
    await pane.getByTestId('composer-send').click();
    await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
    const session=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
    assert(!sessions.includes(session));sessions.push(session);if(sessions.length%16===0)console.log(JSON.stringify({durableSessions:sessions.length}));
  }
  await page.getByLabel('Navigation view',{exact:true}).selectOption('flat');
  await page.getByLabel('Conversation order',{exact:true}).selectOption('manual');
  await page.getByRole('button',{name:'Next 64',exact:true}).waitFor();
  await page.waitForFunction(()=>document.querySelector('.manual-pagination')?.textContent.includes('/ 65'));
  await page.waitForFunction(()=>document.querySelectorAll('#sessions [data-testid=conversation-row]').length===64);
  assert.equal(await page.locator('#sessions [data-testid=conversation-row]').count(),64);
  await page.getByRole('button',{name:'Next 64',exact:true}).click();
  await page.locator(`#sessions [data-session-id="${sessions[0]}"]`).waitFor();
  assert.equal(await page.locator('#sessions [data-testid=conversation-row]').count(),1);
  await page.locator(`#sessions [data-session-id="${sessions[0]}"] [data-testid=conversation-open]`).focus();
  await page.keyboard.press('Alt+Home');
  await page.waitForFunction(id=>document.querySelectorAll('#sessions [data-testid=conversation-row]').length===64 && document.querySelector('#sessions [data-testid=conversation-row]')?.dataset.sessionId===id,sessions[0]);
  await page.locator('#sessions [data-testid=conversation-open]').first().focus();await page.keyboard.press('Alt+ArrowDown');
  await page.waitForFunction(id=>document.querySelectorAll('#sessions [data-testid=conversation-row]')[1]?.dataset.sessionId===id,sessions[0]);
  checks.push({completeMembership:65,crossPageKeyboard:true,firstMoveInitializesFromAllMembers:true});
  const peer=await context.newPage();await peer.goto(service.origin);await peer.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await peer.locator('#workbench').waitFor({state:'visible'});
  await peer.waitForFunction(id=>document.querySelectorAll('#sessions [data-testid=conversation-row]')[1]?.dataset.sessionId===id,sessions[0]);
  const dragged=page.locator(`#sessions [data-session-id="${sessions[0]}"]`),target=page.locator(`#sessions [data-session-id="${sessions[64]}"]`);
  await dragged.dragTo(target);
  await page.waitForFunction(id=>document.querySelectorAll('#sessions [data-testid=conversation-row]').length===64 && document.querySelector('#sessions [data-testid=conversation-row]')?.dataset.sessionId===id,sessions[0]);
  await peer.waitForFunction(id=>document.querySelector('#sessions [data-testid=conversation-row]')?.dataset.sessionId===id,sessions[0]);
  checks.push({crossWindowOrder:true,realDrag:true});
  await peer.close();
  for(const theme of ['light','dark'])for(const [width,height] of [[1440,900],[1024,768],[767,900],[390,844]]){
    await page.emulateMedia({colorScheme:theme});await page.setViewportSize({width,height});
    if(width<1024){await page.getByRole('button',{name:'Toggle navigation',exact:true}).click();await page.getByRole('dialog',{name:'Workspace navigation',exact:true}).waitFor();}
    await page.waitForFunction(()=>document.querySelector('[aria-label=\"Navigation view\"]')?.value==='flat' && document.querySelector('[aria-label=\"Conversation order\"]')?.value==='manual' && document.querySelector('.manual-pagination') && document.querySelectorAll('#sessions [data-testid=conversation-row]').length===64);
    const geometry=await page.evaluate(()=>{const list=document.querySelector('#sessions'),sidebar=list.closest('.sidebar');return {overflow:document.documentElement.scrollWidth-innerWidth,sidebarOverflow:sidebar.scrollWidth-sidebar.clientWidth,listOverflow:list.scrollWidth-list.clientWidth,direction:getComputedStyle(list).flexDirection,manualRows:list.querySelectorAll('[data-testid=conversation-row]').length,blankTitles:[...list.querySelectorAll('[data-testid=conversation-open] strong')].filter(row=>!row.textContent.trim()).length};});
    assert(geometry.overflow<=1,JSON.stringify(geometry));assert.equal(geometry.manualRows,64);assert.equal(geometry.blankTitles,0);assert.equal(geometry.direction,'column');assert(geometry.sidebarOverflow<=1,JSON.stringify(geometry));assert(geometry.listOverflow<=1,JSON.stringify(geometry));
    await page.screenshot({path:join(report,`navigation-${width}-${theme}.png`)});checks.push({width,height,theme,...geometry});
    if(width<1024)await page.keyboard.press('Escape');
  }
  await page.setViewportSize({width:1440,height:900});
  await page.getByLabel('Conversation order',{exact:true}).selectOption('updated');
  await page.locator('.manual-pagination').waitFor({state:'detached'});
  await page.getByLabel('Navigation view',{exact:true}).selectOption('workspace');
  for(let index=0;index<64;index++) {
    const directory=join(service.workspace,`sibling-${String(index).padStart(2,'0')}`);await mkdir(directory);
    if(!await page.locator('.workspace-add').evaluate(node=>node.open))await page.locator('.workspace-add summary').click();
    await page.getByLabel('Server directory').fill(directory);await page.getByRole('button',{name:'Add workspace',exact:true}).click();
    await page.waitForFunction(count=>document.querySelectorAll('#workspaces [data-testid=workspace-open]').length===count,index+2);
  }
  assert.equal(await page.locator('#workspaces [data-testid=workspace-open]').count(),65);
  const sourceWorkspace=page.locator('.workspace-row').filter({has:page.getByRole('button',{name:'New conversation in sibling-63',exact:true})});
  await sourceWorkspace.locator('.workspace-toggle').focus();await page.keyboard.press('Alt+Home');
  await page.waitForFunction(()=>document.querySelector('#workspaces .workspace-row .workspace-toggle[title$="sibling-63"]')===document.querySelector('#workspaces .workspace-row .workspace-toggle[title*="sibling-"]'));
  checks.push({completeWorkspaces:65,workspaceKeyboard:true});
  await page.getByLabel('Navigation view',{exact:true}).selectOption('workspace_tree');
  await page.locator('.workspace-location').waitFor();
  await page.screenshot({path:join(report,'workspace-tree.png')});
  await writeFile(join(report,'bridge-errors.json'),JSON.stringify(await page.evaluate(()=>window.bridgeErrors),null,2));
  assert.deepEqual(errors,[]);assert.equal(await page.locator('#notice:visible').count(),0);
  await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,browser:browser.version(),provider:'isolated deterministic fixture',sessions:65,checks,pageErrors:errors},null,2)+'\n');
  console.log(JSON.stringify({sessions:65,checks:checks.length,pageErrors:errors}));
}catch(error){if(page){await writeFile(join(report,'bridge-errors.json'),JSON.stringify(await page.evaluate(()=>window.bridgeErrors).catch(()=>[]),null,2));await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),`${error.stack}\n${await page.locator('body').innerText().catch(()=>'(closed)')}`);}throw error;}
finally{await browser?.close();await service?.close();}
