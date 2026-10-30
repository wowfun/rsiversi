import {workspaceForm} from './controls.mjs';
import {paired} from './paired-env.mjs';
import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
import {detailMode, navigationFilter, details, closeDetails} from './controls.mjs';

const report = process.env.RSI_WEB_REPORT;
assert(report, 'Set a fresh RSI_WEB_REPORT directory');
await mkdir(report, {recursive:false});
await writeFile(join(report,'build-receipt.json'),JSON.stringify(paired.receipt,null,2));
const browser = await chromium.launch({headless:true});
let service, page;
const errors = [], captures = [];
try {
  service = await startService({binary:process.env.RSI_WEB_BINARY, assets:process.env.RSI_WEB_ASSETS, report});
  const context = await browser.newContext({ignoreHTTPSErrors:true, viewport:{width:1440,height:980}});
  page = await context.newPage();
  page.setDefaultTimeout(30000);
  page.on('pageerror', error => errors.push(error.message));
  const receipt = JSON.stringify(service.register('settlement lifecycle'));
  await page.goto(service.origin);
  await page.getByLabel('Device registration receipt').fill(receipt);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.locator('#workbench').waitFor({state:'visible'});
  await workspaceForm(page);
  await page.getByLabel('Directory path').fill(service.workspace);
  await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();
  await page.locator('#workspaces [data-testid=workspace-open]').click();
  await detailMode(page,'verbose');
  const pane = page.locator('[aria-label="Main conversation"]');
  const transcript = pane.locator('.transcript');
  await pane.getByRole('textbox',{name:'Main message',exact:true}).fill('SETTLEMENT_OUTPUT_LIMIT');
  await pane.getByTestId('composer-send').click();
  await transcript.getByText('流式预览 🦀 · first boundary',{exact:true}).waitFor();
  const session = (await pane.getAttribute('data-session-id'));
  const capture = async label => {
    for (const [size,viewport] of [['desktop',{width:1440,height:980}],['narrow',{width:390,height:844}]]) {
      await page.setViewportSize(viewport);
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const measurement = await page.evaluate(() => ({width:innerWidth,documentWidth:document.documentElement.scrollWidth}));
      assert(measurement.documentWidth <= measurement.width, JSON.stringify(measurement));
      await page.screenshot({path:join(report,`${label}-${size}.png`),fullPage:true});
      captures.push({label,size,...measurement});
    }
    await page.setViewportSize({width:1440,height:980});
  };
  const reopen = async () => {
    await page.reload();
    await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();
    await page.locator('#workbench').waitFor({state:'visible'});
    await navigationFilter(page,'all');
    await page.locator('#refresh').click();
    const workspace = page.locator('#workspaces .workspace-toggle');
    await workspace.waitFor();
    if(await workspace.getAttribute('aria-expanded') !== 'true') await workspace.click();
    await page.locator(`[data-testid=conversation-row][data-session-id="${session}"]`).getByTestId('conversation-open').click();
    await pane.locator('.pane-session').filter({hasText:session}).waitFor({state:'attached'});
    await detailMode(page,'verbose');
  };
  await capture('streaming');
  assert.equal(await transcript.locator('.message.assistant').filter({hasText:'first boundary'}).getByRole('button',{name:'Inspect sources',exact:true}).count(),0,'live output has no durable source');
  await reopen();
  await transcript.getByText('流式预览 🦀 · first boundary',{exact:true}).waitFor();
  await capture('streaming-refreshed');
  service.provider.release('SETTLEMENT_OUTPUT_LIMIT');
  await transcript.getByText('Output limit',{exact:false}).first().waitFor();
  const answer = transcript.locator('.message.assistant').filter({hasText:'first boundary'});
  await answer.getByText('保留的部分输出 · final boundary',{exact:false}).waitFor();
  assert.equal(await answer.count(),1);
  assert.equal((await answer.innerText()).split('first boundary').length-1,1);
  await capture('max-tokens');
  await answer.getByRole('button',{name:'Inspect sources',exact:true}).click();
  const source = details(page).locator('.block-sources');
  await source.waitFor();
  assert.equal(await source.locator('.source-reference').count(),1);
  await page.screenshot({path:join(report,'settled-source.png'),fullPage:true});
  await closeDetails(page);
  await reopen();
  await transcript.getByText('Output limit',{exact:false}).first().waitFor();
  assert.equal((await transcript.innerText()).split('first boundary').length-1,1);
  await capture('max-tokens-refreshed');
  assert.equal(service.provider.requests.length,1);
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({browser:browser.version(),session,captures,modelRequests:1,errors},null,2));
} catch (error) {
  if(page) {
    await writeFile(join(report,'failure.html'),await page.content()).catch(()=>{});
    await page.screenshot({path:join(report,'failure.png'),fullPage:true}).catch(()=>{});
  }
  await writeFile(join(report,'failure.json'),JSON.stringify({message:String(error),errors,captures},null,2));
  throw error;
} finally {
  await browser.close();
  await service?.close();
}
