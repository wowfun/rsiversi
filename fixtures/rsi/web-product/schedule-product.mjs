import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {chromium} from 'playwright';
import {join, resolve} from 'node:path';
import {startService} from './service.mjs';
import {openBrowserPage,connectHostOwner,connectWorkbench,chooseStartingWorkspace,registerFixtureWorkspace} from './browser-fixture.mjs';
import {setSessionArchived} from './controls.mjs';
import {cleanupAll} from './cleanup.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const service=await startService({report,binary:resolve(process.env.RSI_WEB_BINARY),assets:resolve(process.env.RSI_WEB_ASSETS),
  localWeb:true,foregroundHost:true,developmentHttp:process.platform==='win32'});
const errors=[];
let page,browser;
try {
  ({page,browser}=await openBrowserPage(chromium,errors));
  const registration=service.register('Schedule creation card');
  await registerFixtureWorkspace(page.request,service,registration);
  await connectHostOwner(page,service);
  await chooseStartingWorkspace(page,service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});
  {
    const manager=page.getByRole('region',{name:'Automation tasks',exact:true});
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    await manager.getByRole('region',{name:'Recommended'}).getByRole('button',{name:'Weekly review',exact:true}).click();
    await manager.waitFor({state:'hidden'});
    await page.waitForFunction(()=>document.querySelector('textarea[aria-label="Main message"]')?.value.includes('Weekly review'));
    assert.equal(service.provider.requests.length,0);
  }
  const observer = await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
  const catalogPage = await observer.newPage();
  catalogPage.on('pageerror', error => errors.push(error.message));
  await connectWorkbench(catalogPage,service,'Independent automation observer');
  await catalogPage.getByRole('button',{name:'Automation tasks',exact:true}).click();
  const openCatalog = catalogPage.getByRole('region',{name:'Automation tasks',exact:true});
  await openCatalog.getByText('No active automation tasks',{exact:true}).waitFor();
  await pane.getByRole('textbox',{name:'Main message'}).fill('Schedule the weekly review fixture.');
  await pane.getByTestId('composer-send').click();
  const transcript=pane.getByRole('region',{name:'Conversation transcript'});
  const card=transcript.locator('[data-turn-tail]').getByRole('button',{name:'Open automation task: Review the weekly activity'});
  await card.waitFor();
  assert.equal(await transcript.locator('.message.tool:visible').count(),0);
  await page.screenshot({path:join(report,'creation.png')});
  await card.click();
  const details=page.getByRole('region',{name:'Automation task details'});
  await details.getByText('Review the weekly activity',{exact:true}).waitFor();
  await page.screenshot({path:join(report,'details.png')});
  const facts=await service.readSessionFacts(await pane.getAttribute('data-session-id'));
  assert.equal(facts.filter(f=>f.type==='tool_intent' && f.name==='schedule_create').length,1);
  assert.equal(facts.filter(f=>f.type==='turn_terminal').length,1);
  try {
    await openCatalog.getByRole('button',{name:/Review the weekly activity/}).waitFor({timeout:8_000});
    await catalogPage.screenshot({path:join(report,'independent-observer.png')});
  } catch (error) {
    await catalogPage.screenshot({path:join(report,'independent-observer.png')});
    await writeFile(join(report,'independent-observer.txt'),await openCatalog.innerText());
    throw error;
  }
  const session=await pane.getAttribute('data-session-id');
  await openCatalog.getByRole('button',{name:'Review the weekly activity',exact:true}).click();
  const linked = openCatalog.getByRole('button',{name:/open linked session$/i});
  assert(await linked.isEnabled());
  assert.equal(await linked.innerText(),session);
  await setSessionArchived(page, session, true);
  const notice = openCatalog.getByText('The linked session is archived.',{exact:true});
  await notice.waitFor({timeout:8000});
  assert(await linked.isDisabled());
  await catalogPage.screenshot({path:join(report,'observed-archive.png')});
  await setSessionArchived(page, session, false);
  await notice.waitFor({state:'hidden',timeout:8000});
  assert(await linked.isEnabled());
  await observer.close();
  {
    await page.getByTestId('new-conversation').click();
    const editor=pane.getByRole('textbox',{name:'Main message'});
    await editor.fill('Keep my existing notes.');
    const manager=page.getByRole('region',{name:'Automation tasks',exact:true});
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    const task=manager.getByRole('button',{name:/Review the weekly activity/});
    await task.waitFor();
    assert.equal(await page.locator('[data-turn-tail]').count(),0);
    await page.screenshot({path:join(report,'task-manager.png')});
    await manager.getByRole('button',{name:'Ended',exact:true}).click();
    await manager.getByText('No ended automation tasks').waitFor();
    await manager.getByRole('button',{name:'Enabled',exact:true}).click();
    const search=manager.getByRole('searchbox',{name:'Search tasks'});
    await search.fill('no such task');await manager.getByText('No matching tasks').waitFor();
    await search.fill(session+' ｗｋｌ');
    await task.waitFor();
    await search.fill('weekly');
    await task.click();
    await manager.getByRole('region',{name:'Automation task details'}).getByText('Review the weekly activity').waitFor();
    await page.setViewportSize({width:420,height:850});
    await page.screenshot({path:join(report,'task-manager-narrow.png')});
    await manager.getByRole('button',{name:/open linked session$/i}).click();
    await pane.locator('[data-turn-tail]').getByRole('button',{name:/Open automation task/}).waitFor();
    assert.equal(await pane.getAttribute('data-session-id'),session);
    await page.setViewportSize({width:1440,height:980});
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    await manager.getByRole('button',{name:'Create in a session'}).click();
    await manager.waitFor({state:'hidden'});
    await page.waitForFunction(()=>document.querySelector('textarea[aria-label="Main message"]')?.value==='Keep my existing notes.\nPlease create an automation task based on the requirements.');
    assert.equal(service.provider.requests.length,2);
    await page.getByRole('button',{name:'Automation tasks',exact:true}).click();
    await manager.getByRole('button',{name:'Review the weekly activity',exact:true}).click();
    const actions = manager.getByRole('button',{name:'Automation task actions'});
    await actions.click();
    await page.getByRole('menuitem',{name:'Delete task'}).click();
    const confirmation = page.getByRole('dialog',{name:'Delete this task?'});
    await confirmation.getByRole('button',{name:'Cancel'}).click();
    assert(await actions.evaluate(button=>button===document.activeElement));
    await actions.click();
    await page.getByRole('menuitem',{name:'Delete task'}).click();
    await confirmation.getByRole('button',{name:'Confirm deletion'}).click();
    await manager.getByText('No active automation tasks',{exact:true}).waitFor();
    const success=page.getByRole('alert').filter({hasText:'Task deleted.'});
    await success.waitFor();
      assert(await success.locator('svg use').evaluate(symbol=>symbol.getBBox().width>0));
    await manager.getByRole('button',{name:'Close',exact:true}).click();
    await manager.waitFor({state:'hidden'});
    assert(await success.isVisible());
    assert.equal(service.provider.requests.length,2);
  }
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({version:browser.version(),model:'isolated replay',
    requests:service.provider.requests.length,foldedProcess:true,matchedReceipt:true,openedDetails:true,
    globalCatalog:true,anotherDeviceSawCreationWhileOpen:true,anotherDeviceSawArchiveAndRestore:true,sessionAndFuzzyTitleSearch:true,
    backgroundTranscriptUnmounted:true,recommendationUnsent:true,existingDraftPreserved:true,
    confirmedDeletionWithoutAnotherModelRequest:true,deletionBannerSurvivedManagerClose:true},null,2));
} catch(error) {
  if(page) {
    await page.screenshot({path:join(report,'failure.png')});
    await writeFile(join(report,'failure.txt'),await page.locator('body').innerText());
  }
  throw error;
} finally { await cleanupAll(()=>browser?.close(),()=>service.close()); }
