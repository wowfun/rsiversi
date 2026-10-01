import {closeDetails,detailMode,resources} from "./controls.mjs";
// Explicit opt-in: actual DeepSeek, real MCP transport, durable CAS and product UI.
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture} from './live-fixture.mjs';
import {startMcpFixture} from './mcp-fixture.mjs';
import {connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';

const mcp = await startMcpFixture({requireCredential:false, templates:true, images:true});
let live, browser, page;
const errors = [];
try {
  live = await liveFixture({configure: async ({config}) => {
    const path = join(config, 'settings.json');
    const settings = JSON.parse(await readFile(path, 'utf8'));
    settings['rsi.mcp'] = {servers:[{id:'fixture',enabled:true,tools:['echo'],resource_templates:true,transport:{kind:'streamable_http',url:mcp.url}}]};
    await writeFile(path, JSON.stringify(settings));
  }});
  browser = await chromium.launch();
  const context = await browser.newContext({ignoreHTTPSErrors:true, viewport:{width:1440,height:900}, deviceScaleFactor:1, colorScheme:'light'});
  page = await context.newPage();
  page.setDefaultTimeout(30000);
  page.on('pageerror', error => errors.push(error.message));
  await connectWorkbench(page, live.service, 'MCP templates and images live');
  await openWorkspace(page, live.service);
  await detailMode(page,'verbose');
  const pane = page.getByRole('region', {name:'Main conversation', exact:true});
  await pane.getByRole('textbox', {name:'Main message',exact:true}).fill('Run this isolated MCP test. Use mcp_resource_read with server fixture and id template:0, parameters {"item":"record","query":"hello world"}. Then call the advertised MCP echo tool once with message MCP_IMAGE_REQUEST. Do not call any other tools. Do not attempt to inspect image pixels; report the text markers returned by both tools. Finally reply MCP_LIVE_DONE.');
  await pane.getByTestId('composer-send').click();
  const deadline = Date.now() + 180000;
  while (Date.now() < deadline) {
    const review = pane.locator('.pending button').filter({hasText:'Review:'});
    if (await review.count() && await review.first().isVisible()) {
      await review.first().click();
      await page.getByRole('button', {name:'Allow once',exact:true}).click();
    }
    if (['Completed','Failed'].includes(await pane.locator('.pane-status').innerText())) break;
    await page.waitForTimeout(100);
  }
  assert.equal(await pane.locator('.pane-status').innerText(), 'Completed');
  const transcript = await pane.locator('.transcript').innerText();
  assert.match(transcript, /MCP_LIVE_DONE/);
  assert.match(transcript, /MCP_TEMPLATE_CONFIRMED/);
  assert.match(transcript, /MCP_IMAGE_CONFIRMED/);
  assert.equal(mcp.evidence.calls, 1);
  assert.equal(live.service.provider.requests.length, 0, 'no mock model calls');
  const session = (await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
  const records = live.service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'], {input:':history\n'.repeat(8)+':exit\n'}).stdout.trim().split('\n').map(line => JSON.parse(line));
  const facts = records.flatMap(record => record.fact ? [record.fact] : []).sort((a,b) => a.seq - b.seq);
  assert(facts.length > 0 && facts[0].seq === 1);
  const echo = facts.find(fact => fact.type === 'tool_result' && fact.result?.value?.tool === 'echo');
  assert(echo && !echo.result.is_error);
  assert.deepEqual(echo.result.content.map(item => item.type), ['text','image','text']);
  assert.equal(echo.result.content[1].media.mime, 'image/png');
  const template = facts.find(fact => fact.type === 'tool_result' && fact.result?.value?.resource?.id === 'template:0');
  assert.equal(template?.result.value.resource.uri, 'fixture://catalog/record?query=hello%20world');
  const tool = pane.locator('article.message').filter({has:page.locator('.message-title').filter({hasText:/^mcp__/})}).first();
  await tool.scrollIntoViewIfNeeded();
  await tool.getByRole('button', {name:'Inspect sources',exact:true}).click();
  await page.locator('.source-reference').filter({hasText:'tool_image'}).click();
  await page.getByRole('button', {name:'Preview source image',exact:true}).click();
  await page.waitForFunction(() => document.querySelector('.image-preview')?.naturalWidth === 80);
  const checks = [];
  for (const theme of ['light','dark']) {
    // System color-scheme follows the real preference default; no DOM style injection.
    await page.emulateMedia({colorScheme:theme});
    for (const [width,height] of [[1440,900],[1024,768],[767,900],[390,844]]) {
      await page.setViewportSize({width,height});
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await resources(page);
      await page.locator('.image-preview').waitFor({state:'visible'});
      const geometry = await page.evaluate(() => ({overflow:document.documentElement.scrollWidth-innerWidth,image:document.querySelector('.image-preview')?.getBoundingClientRect().toJSON(),naturalWidth:document.querySelector('.image-preview')?.naturalWidth}));
      assert(geometry.overflow <= 1, JSON.stringify(geometry));
      assert.equal(geometry.naturalWidth, 80);
      assert(geometry.image.width>0&&geometry.image.height>0,JSON.stringify(geometry));
      checks.push({theme,width,height,...geometry});
      await page.screenshot({path:join(live.report,`${width}-${height}-${theme}.png`)});
    }
  }
  await assertNoNotices(page);
  assert.deepEqual(errors, []);
  await writeFile(join(live.report,'facts.json'), JSON.stringify(facts,null,2));
  await writeFile(join(live.report,'transcript.txt'), transcript);
  await page.setViewportSize({width:1440,height:900});
  await page.waitForFunction(()=>!document.querySelector('.resource-dock')?.classList.contains('resource-fullscreen'));
  await resources(page);
  await closeDetails(page);
  await page.locator('#sign-out').click();
  await page.locator('#login').waitFor({state:'visible'});
  await writeFile(join(live.report,'result.json'), JSON.stringify({status:'passed',model:live.model,browser:browser.version(),session,mcp:mcp.evidence,checks,mock_model_requests:0,clean_sign_out:true},null,2));
} catch (error) {
  if (page && live) await page.screenshot({path:join(live.report,'failure.png')}).catch(()=>{});
  throw new Error(live ? live.redact(error) : String(error));
} finally {
  try { await browser?.close(); } finally { try { await live?.close(); } finally { await mcp.close(); } }
}
