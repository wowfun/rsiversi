import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Real document/Worker/Host closed-review acceptance; provider is deterministic.
import assert from 'node:assert/strict';
import http from 'node:http';
import {once} from 'node:events';
import {readFile,writeFile,mkdir,copyFile,chmod} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
import {assertNoNotices} from './task-checks.mjs';
const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS;
assert(report && assets,'explicit report and built assets required');
await mkdir(report,{recursive:false});
const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY??resolve('target/debug/rsi'),binary);await chmod(binary,0o700);
await writeFile(join(report,'binary.json'),JSON.stringify({sha256:createHash('sha256').update(await readFile(binary)).digest('hex')}));
const results=[];
function response(call) {
  const delta=call?{role:'assistant',tool_calls:[{index:0,id:call.id,type:'function',function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]}:{role:'assistant',content:'REVIEW_SETTLED'};
  return `data: ${JSON.stringify({choices:[{delta,finish_reason:null}]})}\n\ndata: ${JSON.stringify({choices:[{delta:{},finish_reason:call?'tool_calls':'stop'}],usage:{prompt_tokens:20,completion_tokens:10}})}\n\ndata: [DONE]\n\n`;
}
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]) {
  if(process.env.RSI_WEB_BROWSER && process.env.RSI_WEB_BROWSER!==name)continue;
  const path=join(report,name);await mkdir(path);const requests=[];
  const provider=http.createServer(async(req,res)=>{
    try {
      const chunks=[];for await(const chunk of req)chunks.push(chunk);const body=JSON.parse(Buffer.concat(chunks));requests.push(body);
      const messages=body.messages;const reviewed=messages.find(m=>m.tool_call_id==='review');
      const saved=messages.find(m=>m.tool_call_id==='save');
      const call=reviewed?null:saved?{id:'review',name:'request_plan_execution',arguments:{plan_ref:JSON.parse(saved.content).plan_ref}}:{id:'save',name:'plan_write',arguments:{title:'Review exact plan · 中文',body:'Read the isolated source, verify the result, then report.\n\n<em>Literal plan text</em>\nNo external changes.'}};
      res.writeHead(200,{'content-type':'text/event-stream'});res.end(response(call));
    }catch(error){res.writeHead(500).end(String(error));}
  });
  let service,browser,page;const errors=[];
  try {
  provider.listen(0,'127.0.0.1');await once(provider,'listening');
  service=await startService({binary,assets,report:path,configure:async({config})=>{
    const file=join(config,'host-profiles/fixture/host.profile.toml');
    await writeFile(file,(await readFile(file,'utf8')).replace(/endpoint = "[^"]+"/,`endpoint = "http://127.0.0.1:${provider.address().port}"`));
  }});
  ({browser, page} = await openBrowserPage(engine, errors));
  await connectWorkbench(page, service, 'closed plan review');
  await openWorkspace(page, service);
    const pane=page.getByRole('region',{name:'Main conversation',exact:true}),input=pane.getByRole('textbox',{name:'Main message',exact:true});
    await input.fill('/plan on');await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await pane.locator('.command-receipt').filter({hasText:'Draft changed'}).waitFor();assert.equal(requests.length,0);
    await input.fill('Review the exact saved plan.');await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
    await page.getByRole('region',{name:'Needs attention',exact:true}).getByRole('button',{name:'Answer question 1',exact:true}).click();
    const dialog=page.locator('#detail');await dialog.getByText('Review plan',{exact:true}).waitFor();
    assert.match(await dialog.locator('.review-plan').innerText(),/<em>Literal plan text<\/em>/);assert.equal(await dialog.locator('.review-plan em').count(),0);
    assert.equal(await dialog.getByRole('button',{name:'Send answers',exact:true}).count(),0);
    for(const label of ['Approve and execute','Request changes','Decline and end turn'])assert(await dialog.getByRole('button',{name:label,exact:true}).isEnabled());
    await page.screenshot({path:join(path,'review-wide.png')});
    await page.setViewportSize({width:420,height:860});await dialog.getByRole('button',{name:'Approve and execute',exact:true}).scrollIntoViewIfNeeded();
    assert(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth+1));await page.screenshot({path:join(path,'review-narrow.png')});
    await dialog.getByLabel('Optional review feedback',{exact:true}).fill('Verified in the actual browser.');
    await dialog.getByRole('button',{name:'Approve and execute',exact:true}).click();await dialog.waitFor({state:'hidden'});await pane.locator('.pane-status').filter({hasText:'Completed'}).waitFor();
    assert.equal(requests.length,3);assert(JSON.stringify(requests[2].messages).includes('Plan mode is disabled'));
    assert(JSON.stringify(requests[2].messages).includes('Verified in the actual browser.'));
    await page.setViewportSize({width:1440,height:980});await page.screenshot({path:join(path,'approved.png')});await assertNoNotices(page);assert.deepEqual(errors,[]);
    results.push({browser:name,version:browser.version(),closed_choices:true,literal_plan:true,narrow_containment:true,provider_requests:requests.length,approval_commit_visible_to_next_request:true,ok:true});
    await writeFile(join(path,'requests.json'),JSON.stringify(requests,null,2));
  }catch(error){if(page)await page.screenshot({path:join(path,'failure.png')}).catch(()=>{});results.push({browser:name,ok:false,error:String(error),page_errors:errors});throw error;}
  finally {await cleanupAll(() => browser?.close(), () => service?.close(), () => new Promise(resolve=>provider.close(resolve)), () => writeFile(join(report,'results.json'),JSON.stringify(results,null,2)));}
}
console.log(JSON.stringify(results));
