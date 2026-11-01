import {openSettings} from './controls.mjs';
import {paired} from './paired-env.mjs';
import {startService,waitUntil} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {resources} from './controls.mjs';
import {assertNoNotices} from './task-checks.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
const report=process.env.RSI_REPORT_DIR;assert(report);await mkdir(report,{recursive:false});
let service,browser,page;const errors=[],checks=[],skipped=[];
const attributeSettles=(locator,name,expected)=>waitUntil(async()=>{const value=await locator.getAttribute(name);return typeof expected==='string'?value===expected:expected.test(value??'')},name+' settles after layout persistence');
try{
 service=await startService({binary:paired.binary,assets:paired.assets,report});
 await writeFile(join(service.workspace,'panel.txt'),'Independent panel bytes\n');
 browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900},deviceScaleFactor:1});
 await context.addInitScript(()=>{
   const Original=Worker;window.terminalCalls=[];
   window.Worker=class extends Original {
    constructor(...args){super(...args);this.addEventListener('message',({data})=>{
      if(data.kind==='reply'&&window.heldResizeId!==undefined&&data.id===window.heldResizeId)window.heldResizeReply=data;
    })}
    postMessage(data,...rest){
     if(data.kind==='call'&&data.method==='terminal'&&JSON.parse(data.payload).request.type==='resize'&&window.holdTerminalResize&&!window.releaseTerminalResize){
       window.heldResizeId=data.id;window.releaseTerminalResize=()=>super.postMessage(data,...rest);return;
     }
     if(data.kind==='call'&&data.method==='terminal'){try{window.terminalCalls.push(JSON.parse(data.payload).request)}catch{}}
     super.postMessage(data,...rest);
   }};
 });
 page=await context.newPage();page.setDefaultTimeout(15000);page.on('pageerror',error=>errors.push(error.message));
 await connectWorkbench(page,service,'Independent panels');await openWorkspace(page,service);
 const conversation=page.getByRole('region',{name:'Main conversation',exact:true});
 await conversation.getByLabel('Main message',{exact:true}).fill('Create a durable Session for resource verification.');
 await conversation.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
 await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
 await page.getByRole('button',{name:'Workspace files',exact:true}).click();
 const contribution=page.locator('.resource-content .ui-contribution:visible');
 await contribution.getByRole('textbox',{name:'Workspace-relative path',exact:true}).fill('panel.txt');
 await contribution.getByRole('button',{name:'Read file',exact:true}).click();await contribution.locator('pre').filter({hasText:'Independent panel bytes'}).waitFor();
 await page.screenshot({path:join(report,'files-panel.png')});
 const first=await page.locator('[data-dockkit-tab]').first().getAttribute('data-dockkit-tab');
 await openSettings(page);await page.getByRole('dialog',{name:'Settings',exact:true}).waitFor();
 const geometry=await page.getByRole('dialog',{name:'Settings',exact:true}).boundingBox();assert.equal(geometry.width,800);assert.equal(geometry.height,800);
 await page.screenshot({path:join(report,'settings-independent.png')});await page.getByRole('button',{name:'Close settings',exact:true}).click();
 await contribution.locator('pre').filter({hasText:'Independent panel bytes'}).waitFor();checks.push('Settings preserves the resource presentation');
 await page.getByRole('button',{name:'Open resources',exact:true}).first().click();await page.getByRole('button',{name:'Workspace files',exact:true}).click();
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===2);
 await page.getByRole('button',{name:'Split resources',exact:true}).first().click();
 const divider=page.getByRole('separator',{name:'Resize resource panes',exact:true});
 await divider.focus();await page.keyboard.press('ArrowRight');await attributeSettles(divider,'aria-valuenow','52');
 assert(await divider.evaluate(el=>el===document.activeElement));
 await page.keyboard.press('Home');await attributeSettles(divider,'aria-valuenow','20');
 await page.keyboard.press('End');await attributeSettles(divider,'aria-valuenow','80');
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();await attributeSettles(divider,'aria-valuenow','20');
 await divider.focus();for(const value of ['30','40','50']){await page.keyboard.press('Shift+ArrowRight');await attributeSettles(divider,'aria-valuenow',value);}
 await page.screenshot({path:join(report,'split-keyboard.png')});checks.push('Split keyboard bounds, focus and product undo');
 const chips=page.locator('[data-dockkit-tab]');const last=chips.last();await last.click({button:'right'});await page.getByRole('menuitem',{name:'Float',exact:true}).click();
 await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 const move=page.getByRole('button',{name:/^Move resource window:/}),resize=page.getByRole('button',{name:'Resize resource window',exact:true});
 await move.focus();await page.keyboard.press('Home');await attributeSettles(move,'aria-description',/Position 8, 8 pixels/);await page.keyboard.press('Shift+ArrowRight');await attributeSettles(move,'aria-description',/Position 58, 8 pixels/);await page.keyboard.press('Shift+ArrowDown');
 await attributeSettles(move,'aria-description',/Position 58, 58 pixels/);assert(await move.evaluate(el=>el===document.activeElement));
 await resize.focus();await page.keyboard.press('Home');await attributeSettles(resize,'aria-description',/220 by 140 pixels/);await page.keyboard.press('Shift+ArrowRight');await attributeSettles(resize,'aria-description',/270 by 140 pixels/);await page.keyboard.press('Shift+ArrowDown');
 await attributeSettles(resize,'aria-description',/270 by 190 pixels/);assert(await resize.evaluate(el=>el===document.activeElement));
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();await attributeSettles(resize,'aria-description',/270 by 140 pixels/);
 await resize.focus();await page.keyboard.press('Shift+ArrowDown');await attributeSettles(resize,'aria-description',/270 by 190 pixels/);
 await page.screenshot({path:join(report,'two-files-float.png')});checks.push('Float keyboard position, size, focus and product undo');
 await page.getByRole('button',{name:'Dock resource',exact:true}).click();
 await page.locator(`[data-dockkit-tab="${first}"]`).click();await contribution.locator('pre').filter({hasText:'Independent panel bytes'}).waitFor();checks.push('Two independent file views survive split, float and dock');
 await page.locator(`[data-dockkit-tab="${first}"]`).click({button:'right'});await page.getByRole('menuitem',{name:'Close resource tab',exact:true}).click();
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();await page.locator(`[data-dockkit-tab="${first}"]`).waitFor();checks.push('Close undo reopens resource coordinates');
 const rosterCalls=()=>page.evaluate(()=>window.terminalCalls.filter(call=>call.type==='list').length);
 const beforeOpenRoster=await rosterCalls();
 await page.getByRole('button',{name:'Open resources',exact:true}).first().click();
 if(process.platform==='linux'||process.platform==='win32') {
 await waitUntil(async()=>await rosterCalls()>beforeOpenRoster,'launcher activation lists terminals');
 const beforeCreateRoster=await rosterCalls();
 await page.getByRole('button',{name:'New terminal',exact:true}).click();
 const terminal=page.getByRole('region',{name:'Session terminals',exact:true});
 await terminal.getByRole('status').filter({hasText:'You have control'}).waitFor();
 assert.equal(await rosterCalls(),beforeCreateRoster,'individual terminal creation does not load a hidden roster');
 assert.equal(await terminal.getByRole('button',{name:'Refresh',exact:true}).count(),0);
 if(process.platform==='win32')await terminal.getByText('Current user permissions',{exact:true}).waitFor();
 const shellInput=process.platform==='win32'
   ? "[IO.File]::WriteAllText((Join-Path $PWD 'dock-pty.txt'),'dock-terminal-ok'); Write-Output 'DOCK_TERMINAL_VISIBLE'"
   : "printf 'dock-terminal-ok' > dock-pty.txt; printf 'DOCK_TERMINAL_VISIBLE\\n'";
 await terminal.locator('.xterm-helper-textarea').focus();await page.keyboard.type(shellInput);await page.keyboard.press('Enter');
 await waitUntil(async()=>await readFile(join(service.workspace,'dock-pty.txt'),'utf8').catch(()=>null)==='dock-terminal-ok','real Session terminal input');
 await terminal.locator('.xterm-rows').filter({hasText:'DOCK_TERMINAL_VISIBLE'}).waitFor();
 const terminalTab=page.locator('[data-dockkit-tab]').filter({hasText:'Terminal'}).last();
 const followerCalls=()=>page.evaluate(()=>window.terminalCalls.filter(call=>['attach','create','detach'].includes(call.type)));
 const beforeMove=await followerCalls();
 await terminalTab.click({button:'right'});await page.getByRole('menuitem',{name:'Float',exact:true}).click();
 await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 await terminal.getByRole('status').filter({hasText:'You have control'}).waitFor();
 await page.screenshot({path:join(report,'terminal-floating.png')});
 await page.getByRole('button',{name:'Dock resource',exact:true}).click();await terminalTab.waitFor();
 assert.deepEqual(await followerCalls(),beforeMove,'moving a terminal keeps its original follower');
 const beforeCloseRoster=await rosterCalls();
 await terminalTab.click({button:'right'});await page.getByRole('menuitem',{name:'Close resource tab',exact:true}).click();
 await terminal.waitFor({state:'hidden'});
 await waitUntil(async()=>await rosterCalls()>beforeCloseRoster,'restored launcher lists terminals');
 const beforeRestoreRoster=await rosterCalls();await page.getByRole('button',{name:'Undo layout',exact:true}).click();
 await terminal.getByRole('status').filter({hasText:'Read only'}).waitFor();
 assert.equal(await rosterCalls(),beforeRestoreRoster,'reattachment does not load a hidden roster');
 checks.push('Individual terminal tabs keep listing and refresh in the launcher');
 assert.equal((await followerCalls()).filter(call=>call.type==='create').length,1,'undo attaches and never creates or takes control');
 checks.push('Real terminal follower survives float and dock; close detaches; undo attaches read only');
 await terminal.getByRole('button',{name:'Hide terminal panel',exact:true}).click();
 await terminal.waitFor({state:'hidden'});
 const beforeRoster=await page.locator('[data-dockkit-tab]').count();
 await page.getByRole('button',{name:'Open resources',exact:true}).first().click();
 await page.waitForFunction(count=>document.querySelectorAll('[data-dockkit-tab]').length===count+1,beforeRoster);
 await page.locator('.terminal-list:visible').getByRole('button',{name:/^Terminal 1/}).click();
 await terminal.getByRole('status').filter({hasText:'Read only'}).waitFor();
 assert.equal((await followerCalls()).filter(call=>call.type==='create').length,1);
 checks.push('Retained terminal roster refreshes when reopened and attaches its live shell');
 await terminal.getByRole('button',{name:'Take control',exact:true}).click();
 await terminal.getByRole('status').filter({hasText:'You have control'}).waitFor();
 await page.evaluate(()=>window.holdTerminalResize=true);await page.setViewportSize({width:1024,height:768});
 await page.waitForFunction(()=>!!window.releaseTerminalResize);
 await terminal.locator('.xterm-helper-textarea').focus();await page.keyboard.type('exit 0');await page.keyboard.press('Enter');
 await terminal.getByRole('status').filter({hasText:'Exited 0'}).waitFor();
 await page.evaluate(()=>window.releaseTerminalResize());await page.waitForFunction(()=>window.heldResizeReply!==undefined);
 const delayedResize=await page.evaluate(()=>window.heldResizeReply);
 assert.match(delayedResize.error,/Terminal unavailable/,'the real resize is refused after its shell exits');
 await writeFile(join(report,'delayed-resize.json'),JSON.stringify(delayedResize,null,2));
 assert.deepEqual(await terminal.getByRole('alert').allTextContents(),[],'a stale automatic resize must not leave an exited-terminal error');
 checks.push('A real resize delayed until after exit is refused without leaving a terminal alert');
 assert.equal(await page.locator('.resource-cleanup-notice').count(),0,'Terminal lifecycle does not retain failed cleanup');
 checks.push('Native terminal writes workspace bytes, displays output and exits with code 0');
 } else skipped.push({scenario:'Native terminal execution and follower movement',reason:'Requires Linux or Windows'});
 const geometries=[];
 for(const theme of ['light','dark'])for(const [width,height] of [[1440,900],[1024,768],[767,900],[390,844]]){
   await page.emulateMedia({colorScheme:theme,reducedMotion:'reduce'});await page.setViewportSize({width,height});
   await page.waitForFunction(width=>{const desktop=!!document.querySelector('.workbench>.sidebar');const dock=document.querySelector('.resource-dock');return desktop===(width>=1024)&&!!dock&&dock.classList.contains('resource-fullscreen')===(width<768)},width);
   await resources(page);
   await assertNoNotices(page);
   assert.deepEqual(await page.locator('.resource-dock [role="alert"]:visible').allTextContents(),[]);
   await page.locator('.resource-dock:not([hidden])').waitFor({state:'visible'});
   const geometry=await page.evaluate(()=>{
     const box=selector=>{const rect=document.querySelector(selector)?.getBoundingClientRect();return rect?{x:rect.x,y:rect.y,width:rect.width,height:rect.height}:null};
     const viewport=document.querySelector('.terminal-panel .xterm-viewport');
     const terminalBackground=viewport?{actual:getComputedStyle(viewport).backgroundColor,expected:getComputedStyle(document.documentElement).getPropertyValue('--secondary-surface').trim()}:null;
     const control=document.querySelector('[aria-label=Commands]'),r=control.getBoundingClientRect();return {connectionControlHit:control.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)),overflow:document.documentElement.scrollWidth-innerWidth,resources:box('.resource-dock:not([hidden])'),navigation:box('.workbench>.sidebar,.navigation-rail'),main:box('.workspace-main'),terminalBackground};
   });
   assert(geometry.overflow<=1,JSON.stringify(geometry));
   if(geometry.terminalBackground)assert.equal(geometry.terminalBackground.actual,geometry.terminalBackground.expected,'unused terminal viewport follows the surface theme');
   if(width>=1024)assert(Math.abs(geometry.resources.width-Math.min(width*.45,width-geometry.navigation.width-400))<=2,JSON.stringify(geometry));
   if(width>=768){assert(geometry.connectionControlHit,JSON.stringify(geometry));assert(geometry.main.width>=400,JSON.stringify(geometry));}else assert.equal(geometry.resources.width,width);
   await page.screenshot({path:join(report,`dock-${width}-${theme}.png`)});geometries.push({theme,width,height,...geometry});
 }
 await writeFile(join(report,'geometry.json'),JSON.stringify(geometries,null,2));
 checks.push(`Eight ${process.platform} Chromium DPR 1 viewport/theme checks without document overflow`);
 await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,checks,skipped,errors},null,2));assert.deepEqual(errors,[]);
 console.log(JSON.stringify({family:paired.family,checks,skipped,errors}));
}catch(error){if(page){await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.html'),await page.content());await writeFile(join(report,'failure.txt'),await page.locator('body').innerText().catch(()=>String(error)));}throw error}
finally{await browser?.close();await service?.close()}
