import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir,readFile,appendFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService,waitUntil} from '../web-product/service.mjs';
const require=createRequire(new URL('../web-product/package.json',import.meta.url));
const {chromium,firefox}=require('playwright');
const binary=resolve(process.env.RSI_BINARY??'target/debug/rsi'),assets=process.env.RSI_WEB_ASSETS;
assert(assets);const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});
const results=[];
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]){
  const destination=join(output,name);await mkdir(destination,{recursive:true});
  const providerEvidence=[];
  const service=await startService({binary,assets,report:destination,onRequest(body){providerEvidence.push({tools:body.tools?.map(t=>t.function?.name),results:body.messages?.filter(m=>m.role==='tool')});},async configure({config,workspace}){
    const quote=JSON.stringify;
    await appendFile(join(config,'host-profiles/fixture/host.profile.toml'),`\n[[steps]]\nkind='patch'\ntarget='rsi-acp'\n[steps.config]\ndirectory=${quote(join(config,'../../state/rsi/acp'))}\n[[steps.config.endpoints]]\nid='sdk-agent'\nenabled=true\ncwd=${quote(workspace)}\nsandbox='danger-full-access'\n[steps.config.endpoints.launch]\nprogram=${quote(process.execPath)}\narguments=[${quote(resolve('fixtures/rsi/acp/agent.mjs'))},${quote(workspace)}]\n[steps.config.endpoints.launch.environment]\nFIXTURE_SECRET={kind='literal',value='private-fixture-secret'}\n[[steps.config.endpoints.mcp_servers]]\nname='fixture-mcp'\n[steps.config.endpoints.mcp_servers.launch]\nprogram='/usr/bin/python3'\n[steps.config.endpoints.mcp_servers.launch.environment]\nMCP_SECRET={kind='literal',value='private-fixture-secret'}\n`);
  }});
  const browser=await engine.launch({headless:true});const errors=[];let page;
  try{
    const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(45_000);page.on('pageerror',error=>errors.push(error.message));
    await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} ACP UI`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
    await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
    const native=page.getByRole('region',{name:'Main conversation',exact:true});await native.getByRole('textbox',{name:'Main message'}).fill('Please start an external delegation');await native.getByRole('button',{name:'Send ↗'}).click();
    await native.locator('.pane-status').filter({hasText:'Completed'}).waitFor();await native.getByRole('button',{name:'Open external conversation',exact:true}).waitFor();await page.screenshot({path:join(destination,'delegation.png')});await native.getByRole('button',{name:'Open external conversation',exact:true}).click();
    const pane=page.getByRole('region',{name:'External conversation',exact:true});await pane.waitFor({state:'visible'});
    assert.equal(await page.getByRole('button',{name:'Terminal',exact:true}).isDisabled(),true);
    await pane.getByRole('textbox',{name:'External message'}).fill('permission');await pane.getByRole('button',{name:'Send ↗'}).click();
    await pane.getByRole('heading',{name:'Independent SDK permission'}).waitFor();
    assert.equal(await pane.locator('.external-permission button').count(),4);
    await page.getByRole('region',{name:'Needs attention',exact:true}).getByRole('button',{name:'Review permission 1',exact:true}).click();
    await page.waitForFunction(()=>document.activeElement?.classList.contains('external-permission'));
    assert.equal(await readFile(join(service.workspace,'new-count'),'utf8'),'new\n');
    await page.screenshot({path:join(destination,'permission.png')});
    await pane.getByRole('button',{name:'Always · allow always',exact:true}).click();
    await pane.locator('.external-status').filter({hasText:'completed'}).waitFor();
    await pane.locator('.external-record').filter({hasText:'SDK_AGENT_VERIFIED'}).waitFor();
    assert.equal(await page.evaluate(()=>window.externalExecuted),undefined);
    const record=pane.locator('.external-record').filter({hasText:'SDK_AGENT_VERIFIED'}).last();await record.getByRole('button',{name:'Source',exact:true}).click();await pane.locator('.external-source').filter({hasText:'agent_message_chunk'}).waitFor();
    await page.screenshot({path:join(destination,'conversation.png')});
    await page.setViewportSize({width:430,height:900});await page.screenshot({path:join(destination,'narrow.png')});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),false);await page.setViewportSize({width:1440,height:980});
    await pane.getByRole('textbox',{name:'External message'}).fill('again');await pane.getByRole('button',{name:'Send ↗'}).click();await pane.getByRole('heading',{name:'Independent SDK permission'}).waitFor();await pane.getByRole('button',{name:'Always · allow always',exact:true}).click();await pane.locator('.external-status').filter({hasText:'completed'}).waitFor();
    const before=await readFile(join(service.workspace,'new-count'),'utf8');assert.equal(before,'new\n');
    await pane.getByRole('textbox',{name:'External message'}).fill('retained external draft');
    await page.locator('#workspaces .nav-item').first().click();
    await page.getByRole('region',{name:'Main conversation',exact:true}).waitFor({state:'visible'});
    await page.getByRole('button',{name:'Refresh external conversations',exact:true}).click();await page.locator('.external-navigation .nav-item').first().click();await pane.waitFor({state:'visible'});assert.equal(await pane.getByRole('textbox',{name:'External message'}).inputValue(),'retained external draft');assert.equal(await readFile(join(service.workspace,'new-count'),'utf8'),before);
    await pane.getByRole('button',{name:'Close peer',exact:true}).click();await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();
    await pane.getByRole('button',{name:'Resume connection',exact:true}).click();await pane.locator('.external-status').filter({hasText:'ready'}).waitFor();
    await pane.getByRole('textbox',{name:'External message'}).fill('wait');await pane.getByRole('button',{name:'Send ↗'}).click();await pane.locator('.external-status').filter({hasText:'running'}).waitFor();await pane.getByRole('button',{name:'Cancel prompt',exact:true}).click();await pane.locator('.external-status').filter({hasText:'cancelled'}).waitFor();
    await pane.getByRole('button',{name:'Close peer',exact:true}).click();await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();await pane.getByRole('button',{name:'Reload remote history',exact:true}).click();await pane.locator('.external-status').filter({hasText:'ready'}).waitFor();await pane.locator('.external-record pre').filter({hasText:/^1199$/}).waitFor();
    assert.equal(await pane.locator('.external-record').count(),128);assert.equal(await pane.locator('.external-source').isVisible(),false);await page.screenshot({path:join(destination,'replay.png')});
    await pane.getByRole('button',{name:'Close peer',exact:true}).click();await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();
    const pids=(await readFile(join(service.workspace,'peer-pids'),'utf8')).trim().split('\n');await waitUntil(async()=>{for(const pid of pids){try{process.kill(Number(pid),0);return false;}catch(error){if(error.code!=='ESRCH')throw error;}}return true;},'external process reaping');
    await pane.getByRole('textbox',{name:'External message'}).fill('must disappear on connection retirement');
    await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});
    await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} second principal`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
    await page.locator('.external-navigation .nav-item').first().click();await pane.waitFor({state:'visible'});assert.equal(await pane.getByRole('textbox',{name:'External message'}).inputValue(),'');
    assert.deepEqual(errors,[]);assert.equal(service.provider.requests.length,2);results.push({engine:name,version:browser.version(),permissions:4,replay:1200,retained:128,mock_provider_requests:2,delegation_card:true,peers_reaped:pids.length,ok:true});
  }catch(error){if(page)await page.screenshot({path:join(destination,'failure.png')}).catch(()=>{});results.push({engine:name,ok:false,error:String(error),page_errors:errors});throw error;}
  finally{await writeFile(join(destination,'provider-evidence.json'),JSON.stringify(providerEvidence,null,2)+'\n');await browser.close();await service.close();await writeFile(join(output,'result.json'),JSON.stringify(results,null,2)+'\n');}
}
console.log(JSON.stringify(results));
