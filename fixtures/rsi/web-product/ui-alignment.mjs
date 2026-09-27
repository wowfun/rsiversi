import {paired} from './paired-env.mjs';
import assert from 'node:assert/strict';
import {mkdir,writeFile,stat} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
const report=resolve(process.env.RSI_WEB_REPORT??'target/rsi-web-product/ui-alignment');
await mkdir(report,{recursive:true});
for(const [name,engine] of Object.entries({chromium,firefox})){
  if(process.env.RSI_WEB_BROWSER&&process.env.RSI_WEB_BROWSER!==name)continue;
  const directory=join(report,name);await mkdir(directory,{recursive:true});
  const browser=await engine.launch();
  let service,page;const errors=[],checks=[];
  try{
    service=await startService({binary:paired.binary,assets:paired.assets,report:directory});
    const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900},colorScheme:'dark'});
    if(name==='chromium')await context.grantPermissions(['clipboard-read','clipboard-write']);
    page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
    await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register('UI alignment')));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
    await page.screenshot({path:join(directory,'empty-dark.png')});
    const editor=await page.getByLabel('Main message',{exact:true}).elementHandle();
    await page.getByTestId('choose-workspace').click();
    const dialog=page.getByRole('dialog',{name:'Select workspace directory'});
    await dialog.getByRole('button',{name:'Edit directory path',exact:true}).click();await dialog.getByLabel('Directory path').fill(service.workspace);await dialog.getByRole('button',{name:'Go',exact:true}).click();
    await dialog.getByRole('button',{name:'+ New folder',exact:true}).click();await dialog.getByLabel('Folder name').fill('.');await dialog.getByRole('button',{name:'Create folder',exact:true}).click();await dialog.getByRole('status').filter({hasText:'Invalid directory path or folder name'}).waitFor();assert.equal(await dialog.getByRole('button',{name:'Read parent to check result',exact:true}).count(),0);await dialog.getByLabel('Folder name').fill('ui-folder');await dialog.getByRole('button',{name:'Create folder',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('.directory-column:last-child')?.getAttribute('aria-label')?.endsWith('/ui-folder'));assert.equal(await dialog.locator('.directory-column').count(),2);await page.screenshot({path:join(directory,'directory-dark.png')});
    await dialog.getByRole('button',{name:'Open',exact:true}).click();await dialog.waitFor({state:'detached'});await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
    assert((await stat(join(service.workspace,'ui-folder'))).isDirectory());
    assert(await editor.evaluate(node=>node===document.querySelector('[aria-label="Main message"]')));
    const input=page.getByLabel('Main message',{exact:true});await input.fill('Markdown example');await input.press('Enter');await page.locator('.message.assistant').filter({hasText:'Review notes'}).waitFor();await page.waitForFunction(()=>document.querySelector('.pane-status').textContent==='Completed');
    assert(await editor.evaluate(node=>node===document.querySelector('[aria-label="Main message"]')),'empty and docked composer identity');
    await page.getByRole('button',{name:'Copy code',exact:true}).click();await page.getByRole('button',{name:'Copied',exact:true}).waitFor();
    checks.push({directory:true,create:true,enterSend:true,composerIdentity:true,clipboard:true});
    await page.locator('[data-testid="conversation-row"]').first().waitFor();await page.locator('[data-testid="conversation-row"] .session-options summary').first().click();await page.getByRole('button',{name:'Pin',exact:true}).click();await page.getByRole('region',{name:'Pinned conversations'}).getByTestId('conversation-open').waitFor();
    const refreshGroup = page.getByRole('button', {name:'Refresh conversations in ui-folder',exact:true});
    await refreshGroup.waitFor();await refreshGroup.click();await refreshGroup.waitFor({state:'detached'});
    assert.equal(await page.getByTestId('workspace-group').filter({hasText:'ui-folder'}).getByTestId('conversation-row').count(),0);
    checks.push({pin:true,explicitGroupRefresh:true});
    for(const width of [390,768,1024,1440,1920])for(const theme of ['light','dark','system']){
      await page.setViewportSize({width,height:900});
      // Apply through the real Settings owner; same device preference is also used by Desktop.
      await page.getByRole('button',{name:'Commands',exact:true}).click();await page.getByRole('combobox',{name:'Search commands'}).fill('appearance');await page.getByRole('combobox',{name:'Search commands'}).press('Enter');
      await page.getByRole('combobox',{name:'Settings / appearance / theme',exact:true}).selectOption({label:theme});
      await page.getByRole('button',{name:'Save settings',exact:true}).click();await page.waitForFunction(theme=>document.documentElement.dataset.theme===theme,theme);await page.getByRole('button',{name:'Close details',exact:true}).click();await page.locator('#detail').waitFor({state:'hidden'});
      const geometry=await page.evaluate(()=>{const input=document.querySelector('.pane.selected .composer'),button=document.querySelector('.pane.selected [data-testid="composer-send"]'),r=button.getBoundingClientRect();return{overflow:document.documentElement.scrollWidth-innerWidth,composer:input.getBoundingClientRect().toJSON(),sidebar:document.querySelector('.workbench>.sidebar')?.getBoundingClientRect().width??null,rail:document.querySelector('.workbench>.navigation-rail')?.getBoundingClientRect().width??null,sendHit:button.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),theme:document.documentElement.dataset.theme,scheme:getComputedStyle(document.documentElement).colorScheme}});
      assert(geometry.overflow<=1,JSON.stringify(geometry));assert(geometry.sendHit,JSON.stringify(geometry));assert(geometry.composer.x>=0&&geometry.composer.right<=width,JSON.stringify(geometry));if(width>=1024)assert.equal(geometry.sidebar,280);if(width===768)assert.equal(geometry.rail,56);
      checks.push({width,theme,...geometry});await page.screenshot({path:join(directory,`${width}-${theme}.png`)});
    }
    await page.setViewportSize({width:1440,height:900});
    await input.fill('hold this turn');await input.press('Enter');await page.getByTestId('composer-send').filter({hasText:'Queue'}).waitFor();
    await input.fill('queued survivor');await input.press('Enter');await page.locator('.queue-row').waitFor();await input.fill('draft remains');await page.getByRole('button',{name:'Sending options'}).count();
    await page.getByRole('button',{name:'Stop',exact:true}).click();await page.waitForFunction(()=>document.querySelector('.pane-status').textContent==='Completed');assert.equal(await input.inputValue(),'draft remains');
    checks.push({queue:true,stopKeepsDraft:true});
    await page.screenshot({path:join(directory,'conversation.png')});assert.deepEqual(errors,[]);assert.equal(await page.locator('#notice:visible').count(),0);
    await page.reload();await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});await page.getByRole('region',{name:'Pinned conversations'}).getByTestId('conversation-open').click();await page.waitForFunction(()=>document.querySelector('[aria-label="Main message"]').value==='draft remains');checks.push({reloadReconnect:true,draftRestored:true,pinRestored:true});
    await page.evaluate(()=>{
      const transaction=IDBDatabase.prototype.transaction;
      window.restoreLayoutTransactions=()=>{IDBDatabase.prototype.transaction=transaction};
      IDBDatabase.prototype.transaction=function(...args){if(this.name==='rsi.presentation')throw new DOMException('fixture layout failure','UnknownError');return transaction.apply(this,args)};
    });
    await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'Layout preferences could not be saved'}).waitFor();
    await page.evaluate(()=>window.restoreLayoutTransactions());
    await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
    await page.getByRole('alert').filter({hasText:'Layout preferences could not be saved'}).waitFor({state:'hidden'});
    await page.addInitScript(()=>{
      const transaction=IDBDatabase.prototype.transaction;
      IDBDatabase.prototype.transaction=function(...args){if(this.name==='rsi.presentation')throw new DOMException('fixture layout failure','UnknownError');return transaction.apply(this,args)};
    });
    await page.reload();await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
    await page.getByRole('alert').filter({hasText:'Layout preferences could not be loaded'}).waitFor();
    for(const width of [390,1440]) {
      await page.setViewportSize({width,height:900});
      const geometry=await page.evaluate(()=>{
        const header=document.querySelector('.connected-header').getBoundingClientRect();
        const alert=document.querySelector('.preference-error').getBoundingClientRect();
        const control=document.querySelector('[aria-label="Toggle resources"]'),rect=control.getBoundingClientRect();
        return {headerBottom:header.bottom,alertTop:alert.top,controlHit:control.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2))};
      });
      await page.screenshot({path:join(directory,`alert-${width}.png`)});
      assert(geometry.headerBottom<=geometry.alertTop,`Toolbar overlaps alert at ${width}px: ${JSON.stringify(geometry)}`);
      assert(geometry.controlHit,'alert must not cover toolbar controls');
      checks.push({alertLayout:true,width,...geometry});
    }

    await page.getByRole('region',{name:'Pinned conversations'}).getByTestId('conversation-open').click();
    await page.waitForFunction(()=>document.querySelector('[aria-label="Main message"]').value==='draft remains');
    await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
    checks.push({layoutSaveFailureVisible:true,layoutSaveRecovery:true,layoutLoadFailureVisible:true,independentDraftRestoration:true});
    await page.getByRole('button',{name:'Sign out',exact:true}).click();await page.locator('#workbench').waitFor({state:'hidden'});
    await writeFile(join(directory,'result.json'),JSON.stringify({family:paired.family,browser:await browser.version(),checks,errors},null,2));
  }catch(error){if(page){await page.screenshot({path:join(directory,'failure.png')}).catch(()=>{});await writeFile(join(directory,'failure.html'),await page.content()).catch(()=>{});}await writeFile(join(directory,'failure.json'),JSON.stringify({error:String(error),errors}));throw error;}
  finally{await browser.close();await service?.close();}
}
