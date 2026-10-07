import {paired} from './paired-env.mjs';
import assert from 'node:assert/strict';
import {mkdir,copyFile,chmod,writeFile} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {createHash} from 'node:crypto';
import {join} from 'node:path';
import {chromium,firefox} from 'playwright';
import {openBrowserPage,connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {openResource,resources,details,closeDetails,clickDetails} from './controls.mjs';
import {startService,waitUntil} from './service.mjs';
import {assertNoPendingWorkflowCleanup,configureProgram,nativePrograms,programEvidence} from './program-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';
import {cleanupAll} from './cleanup.mjs';
const dockOnly=process.argv.includes('--dock-only');
assert(process.argv.slice(2).every(arg=>arg==='--dock-only'),'unknown workflow verification argument');
const report=process.env.RSI_WEB_REPORT;
assert(report && process.env.RSI_TEST_NODE,'new report and explicit RSI_TEST_NODE required');
await mkdir(report,{recursive:false});
const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY,binary);await chmod(binary,0o700);
const binaryHash=createHash('sha256');
for await(const chunk of createReadStream(binary))binaryHash.update(chunk);
await writeFile(join(report,'binary.json'),JSON.stringify({sha256:binaryHash.digest('hex')}));
const results=[];
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]) {
  const folder=join(report,name);await mkdir(folder);
  let service,browser,page;const errors=[];
  try {
    service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report:folder,configure:configureProgram});
    ({browser,page}=await openBrowserPage(engine,errors));
    await page.addInitScript(()=>{
      window.workflowInvocations=[];
      const owners=new WeakMap();let next=0;
      const post=Worker.prototype.postMessage;
      Worker.prototype.postMessage=function(message,...rest){
        let owner=owners.get(this);
        if(owner===undefined){
          owner=++next;owners.set(this,owner);
          this.addEventListener('message',({data})=>{
            if(data?.kind!=='reply')return;
            const row=window.workflowInvocations.find(row=>row.owner===owner&&row.id===data.id);
            if(row)row.reply={ok:!data.error,error:data.error?String(data.error).slice(0,4096):null,notAdmitted:data.notAdmitted};
          });
        }
        let request;
        if(message?.method==='command'&&typeof message.payload==='string')try{request=JSON.parse(message.payload);}catch{}
        if(request?.action==='ui_invoke'&&request.name==='workflow'){
          window.workflowInvocations.push({owner,id:message.id,ticket:request.ticket,value:request.input?.value,reply:null});
          if(window.workflowInvocations.length>64)window.workflowInvocations.shift();
        }
        return post.call(this,message,...rest);
      };
    });
    await connectWorkbench(page,service,`workflow ${name}`);await openWorkspace(page,service);
    const pane=page.getByRole('region',{name:'Main conversation',exact:true});
    const input=pane.getByRole('textbox',{name:'Main message',exact:true});
    await openResource(page,'Workflows');
    await details(page).getByText('No accepted workflows on this page.',{exact:false}).waitFor();
    assert.match(await details(page).innerText(),/Runtime: Available/);
    await closeDetails(page);
    await input.fill('WORKFLOW_FIXTURE_COMPLETE');await pane.getByTestId('composer-send').click();
    await waitUntil(()=>programEvidence(service.workspace).controls.some(({record})=>record.type==='program_run'&&record.event.event==='terminal'),'canonical completion',90000);
    await pane.locator('.pane-status').getByText('Completed',{exact:true}).waitFor();
    if (!dockOnly) {
    const invocation=pane.locator('.message-title.summary-toggle').filter({hasText:'run_workflow'}).first();
    if(await invocation.getAttribute('aria-expanded')==='false')await invocation.click();
    await invocation.scrollIntoViewIfNeeded();
    await pane.getByRole('button',{name:'Open workflow',exact:true}).first().waitFor();
    await pane.getByRole('button',{name:'Open workflow',exact:true}).first().click();
    await waitUntil(async()=>/State: Completed/.test(await pane.locator('.inline-card').filter({visible:true}).innerText()),'inline canonical detail');
    await pane.getByRole('button',{name:'Inspect child 1',exact:true}).click();
    await waitUntil(async()=>/report_result/.test(await pane.locator('.inline-card').filter({visible:true}).innerText()),'inline child history');
    await page.screenshot({path:join(folder,'inline-child-wide.png')});
    await pane.getByRole('button',{name:'Latest workflows',exact:true}).click();
    }
    await openResource(page,'Workflows');
    await clickDetails(page,'Open workflow');
    await waitUntil(async()=>/State: Completed/.test(await details(page).innerText()),'canonical completed view');
    await clickDetails(page,'Read result');
    await waitUntil(async()=>/"total":42/.test(await details(page).innerText()),'CAS result');
    await page.screenshot({path:join(folder,'result-wide.png')});
    await clickDetails(page,'Read frozen script');
    await waitUntil(async()=>/workflow.parallel/.test(await details(page).innerText()),'frozen script');
    await clickDetails(page,'Refresh current view');
    await waitUntil(async()=>await details(page).getByRole('button',{name:'Refresh current view',exact:true}).isEnabled()&&/workflow.parallel/.test(await details(page).innerText())&&/State: Completed/.test(await details(page).innerText()),'refresh retains selected script detail');
    await page.screenshot({path:join(folder,'script-wide.png')});
    await clickDetails(page,'Inspect child 1');
    await waitUntil(async()=>/report_result/.test(await details(page).innerText()),'child durable history');
    await page.screenshot({path:join(folder,'child-wide.png')});
    await clickDetails(page,'Latest workflows');
    await closeDetails(page);
    const root=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
    await waitUntil(()=>page.evaluate(()=>window.workflowInvocations.every(row=>row.reply)),'initial Workflow control replies');
    const initialInvocations=await page.evaluate(()=>window.workflowInvocations);
    assert(initialInvocations.length&&initialInvocations.every(row=>row.reply.ok),'initial Workflow controls must acknowledge without errors');
    await writeFile(join(folder,'initial-invocations.json'),JSON.stringify(initialInvocations,null,2));
    await service.restart();
    await page.reload();await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
    const workspace=page.locator('#workspaces .workspace-toggle').first();
    if(await workspace.getAttribute('aria-expanded')==='false')await workspace.click();
    await page.locator(`[data-testid="conversation-row"][data-session-id="${root}"]`).getByTestId('conversation-open').click();
    await page.waitForFunction(root=>document.querySelector('[aria-label="Main conversation"]')?.getAttribute('data-session-id')===root&&!document.querySelector('[aria-label="Main message"]')?.disabled,root);
    await resources(page);
    // Restored Dock tabs can move the same connected launcher into a hidden tab
    // during mount. Resolve and hit-test the current control before a real click.
    let workflowPoint;
    await waitUntil(async()=>{
      workflowPoint=await page.evaluate(()=>{
        const b=[...document.querySelectorAll('.resource-dock .resource-launcher button')].find(b=>b.textContent==='Workflows'&&b.getBoundingClientRect().width>0&&b.getBoundingClientRect().height>0);
        if(!b||b.disabled)return null;
        b.scrollIntoView({block:'center'});const r=b.getBoundingClientRect();
        const x=r.x+r.width/2,y=r.y+r.height/2;
        return b.contains(document.elementFromPoint(x,y))?{x,y}:null;
      });return !!workflowPoint;
    },'current cold Workflows launcher');
    await page.mouse.click(workflowPoint.x,workflowPoint.y);
    await clickDetails(page,'Open workflow');
    await waitUntil(async()=>/State: Completed/.test(await details(page).innerText()),'canonical completed view');
    await page.setViewportSize({width:420,height:900});
    await page.waitForFunction(()=>document.querySelector('.resource-dock')?.classList.contains('resource-fullscreen'));
    await resources(page);
    await page.locator('.resource-dock.resource-fullscreen:not([hidden])').waitFor({state:'visible'});
    await details(page).getByRole('button',{name:'Refresh current view',exact:true}).waitFor();
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
    await page.screenshot({path:join(folder,'history-narrow.png')});
    await closeDetails(page);await page.setViewportSize({width:1440,height:980});
    await input.fill('WORKFLOW_FIXTURE_CANCEL');await pane.getByTestId('composer-send').click();
    await waitUntil(async()=>(await nativePrograms(service)).length===1,'one live Node');
    const identity=(await nativePrograms(service))[0];
    await pane.locator('.pane-status').getByText('Completed',{exact:true}).waitFor();
    await openResource(page,'Workflows');
    await clickDetails(page,'Latest workflows');
    await clickDetails(page,'Open workflow');
    await clickDetails(page,'Cancel workflow');
    await waitUntil(async()=>/State: Cancelled/.test(await details(page).innerText()),'canonical cancelled view');
    assertNoPendingWorkflowCleanup(await details(page).innerText());
    await waitUntil(async()=>!(await nativePrograms(service)).length,'actual Node reaped after cancellation');
    const data=programEvidence(service.workspace);await writeFile(join(folder,'evidence.json'),JSON.stringify(data,null,2));
    const runs=data.controls.filter(({record})=>record.type==='program_run');
    const terminal=runs.filter(({record})=>record.event.event==='terminal');
    assert.deepEqual(terminal.map(({record})=>record.event.outcome.status),['completed','cancelled']);
    assert.equal(runs.filter(({record})=>record.event.event==='child_settled').length,2);
    const intents=data.facts.filter(({record})=>record.type==='tool_intent');
    assert(!intents.some(({record})=>record.name==='workflow_cancel'),'user cancellation did not call the model Tool');
    assert.equal(intents.filter(({record})=>record.name==='report_result').length,2);
    await page.screenshot({path:join(folder,'cancel-wide.png')});
    await waitUntil(()=>page.evaluate(()=>window.workflowInvocations.every(row=>row.reply)),'restored Workflow control replies');
    const invocations=await page.evaluate(()=>window.workflowInvocations);
    assert(invocations.length&&invocations.every(row=>row.reply.ok),'restored Workflow controls must acknowledge without errors');
    await writeFile(join(folder,'restored-invocations.json'),JSON.stringify(invocations,null,2));
    await assertNoNotices(page);assert.deepEqual(errors,[]);
    results.push({scope:dockOnly?'dock':'dock-and-inline',browser:name,version:browser.version(),terminal:terminal.map(({record})=>record.event.outcome.status),node:identity,model_requests:service.provider.requests.length,ok:true});
  } catch(error) {if(service) await writeFile(join(folder,'failure-evidence.json'),JSON.stringify(programEvidence(service.workspace),null,2)).catch(()=>{});if(page){await writeFile(join(folder,'failure.html'),await page.content()).catch(()=>{});await page.evaluate(()=>window.workflowInvocations).then(value=>writeFile(join(folder,'failure-invocations.json'),JSON.stringify(value,null,2))).catch(()=>{});}await page?.screenshot({path:join(folder,'failure.png')}).catch(()=>{});throw error;}
  finally {await cleanupAll(()=>browser?.close(),()=>service?.close());}
}
await writeFile(join(report,'result.json'),JSON.stringify({ok:true,family:paired.family,results},null,2));
