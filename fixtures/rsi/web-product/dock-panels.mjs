import {openSettings} from './controls.mjs';
import {paired} from './paired-env.mjs';
import {startService,waitUntil} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {resourceGuide,resources} from './controls.mjs';
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
 const shellHistoryPath=join(service.workspace,'..','home','AppData','Roaming','Microsoft','Windows','PowerShell','PSReadLine','ConsoleHost_history.txt');
 if(process.platform==='win32'){
   const probe=service.probe('powershell.exe',null,['-NoLogo','-NoProfile','-NonInteractive','-Command','(Get-PSReadLineOption).HistorySavePath']);
   assert.equal(probe.stdout.trim().toLowerCase(),shellHistoryPath.toLowerCase(),'Windows PowerShell uses the fixture profile');
 }
 await writeFile(join(service.workspace,'panel.txt'),'Independent panel bytes\n');
 await mkdir(join(service.workspace,'src'));
 await writeFile(join(service.workspace,'src','a.txt'),'Nested workspace preview\n');
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
 const rosterCalls=()=>page.evaluate(()=>window.terminalCalls.filter(call=>call.type==='list').length);
 assert.equal(await rosterCalls(),0,'the closed resource panel must not request the terminal roster');
 await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
 const activeGuide=page.locator('.resource-dock > .resource-guide, [data-dockkit-pane-active] .resource-guide');
 const more=activeGuide.getByRole('button',{name:'More tools',exact:true});
 await more.waitFor();
 assert.equal(await rosterCalls(),0,'opening the guide requires no shell discovery');
 await page.screenshot({path:join(report,'resource-guide.png')});
 const outer=page.getByRole('separator',{name:'Resize resources',exact:true});
 const outerWidth=async()=>Math.round((await page.getByRole('complementary',{name:'Session resources'}).boundingBox()).width);
 async function dragOuter(button,dx){
   const box=await outer.boundingBox();
   const x=Math.round(box.x+box.width/2),y=Math.round(box.y+box.height/2);
   await page.mouse.move(x,y);await page.mouse.down({button});await page.mouse.move(x+dx,y,{steps:8});await page.mouse.up({button});
 }
 const initialWidth=await outerWidth();
 await dragOuter('right',48);await page.keyboard.press('Escape');
 assert.equal(await outerWidth(),initialWidth,'secondary drag preserves the width');
 await dragOuter('left',76);
 await waitUntil(async()=>await outerWidth()===initialWidth-76,'primary drag applies its release position');
 await page.setViewportSize({width:1600,height:900});
 await page.waitForFunction(()=>window.innerWidth===1600);
 assert.equal(await outerWidth(),initialWidth-76,'widening the viewport retains the preferred pixel width');
 await page.setViewportSize({width:1440,height:900});await dragOuter('left',-76);
 await waitUntil(async()=>await outerWidth()===initialWidth,'restore the original panel width');
 checks.push('Resource width follows primary drags and retains its CSS pixel preference across window resizes');
 const readsBeforeHiding=await rosterCalls();
 const toggleResources=page.getByRole('button',{name:'Toggle resources',exact:true});
 await toggleResources.click();
 await page.locator('.resource-dock:visible').waitFor({state:'hidden'});
 assert.equal(await rosterCalls(),readsBeforeHiding);
 await toggleResources.click();
 await more.waitFor();
 assert.equal(await rosterCalls(),readsBeforeHiding);
 await more.focus();await more.press('Enter');
 const listTerminals=page.getByRole('menuitem',{name:'List terminals',exact:true});
 await listTerminals.waitFor();
 assert.equal(await rosterCalls(),readsBeforeHiding,'the general menu does not read the terminal roster');
 await page.getByRole('menuitem',{name:'Workflows',exact:true}).waitFor();
 await page.screenshot({path:join(report,'resource-guide-menu.png')});
 await listTerminals.click();
 await page.getByRole('status').filter({hasText:'No retained terminals'}).waitFor();
 assert.equal(await rosterCalls(),readsBeforeHiding+1,'one lookup per explicit list');
 const refresh=page.getByRole('menuitem',{name:'Refresh terminals',exact:true});
 await refresh.focus();await refresh.press('Enter');
 await page.getByRole('status').filter({hasText:'No retained terminals'}).waitFor();
 assert.equal(await rosterCalls(),readsBeforeHiding+2,'keyboard refresh reads the list again');
 await page.keyboard.press('Escape');
 assert(await more.evaluate(el=>el===document.activeElement),'Escape returns focus to More tools');
 checks.push('Files, New terminal and More tools need no roster read; explicit listing and refresh each read once');
 await page.getByRole('button',{name:'Workspace files',exact:true}).click();
 const contribution=page.locator('.resource-content .ui-contribution:visible');
 await contribution.getByRole('button',{name:'panel.txt',exact:true}).waitFor();
 await page.locator('[data-dockkit-add-tab]:visible').click();
 const guideTab=page.locator('[data-dockkit-content]:visible .resource-guide');
 await guideTab.getByRole('button',{name:'Workspace files',exact:true}).waitFor();
 const guidePane=page.locator('[data-dockkit-pane]').filter({has:page.locator('.resource-guide')});
 assert.equal(await guidePane.locator('[data-dockkit-add-tab]').count(),0);
 const openGuideChip=guidePane.locator('[data-dockkit-tab][aria-selected=true]');
 await openGuideChip.click({button:'right'});
 await page.getByRole('menuitem',{name:'Float',exact:true}).waitFor();
 assert.equal(await page.getByRole('menuitem',{name:'Duplicate',exact:true}).count(),0);
 await page.keyboard.press('Escape');
 const fillsBody=await guideTab.evaluate(guide=>
   Math.abs(guide.getBoundingClientRect().height-guide.parentElement.clientHeight)<1);
 assert(fillsBody,'the guide fills its tab body');
 await page.locator('[data-dockkit-tab][aria-selected=true]:visible').click({button:'middle'});
 await contribution.getByRole('button',{name:'panel.txt',exact:true}).waitFor();
 await contribution.getByRole('button',{name:'src/',exact:true}).click();
 await contribution.getByRole('button',{name:'a.txt',exact:true}).click();
 await contribution.locator('pre').filter({hasText:'Nested workspace preview'}).waitFor();
 await page.screenshot({path:join(report,'nested-file.png')});
 checks.push('Files opens directly on the selected Session root and previews nested file contents');
 await contribution.getByRole('button',{name:'Release snapshot',exact:true}).click();
 await contribution.getByRole('textbox',{name:'Workspace-relative path',exact:true}).fill('panel.txt');
 await contribution.getByRole('button',{name:'Read file',exact:true}).click();await contribution.locator('pre').filter({hasText:'Independent panel bytes'}).waitFor();
 await page.screenshot({path:join(report,'files-panel.png')});
 const first=await page.locator('[data-dockkit-tab]').first().getAttribute('data-dockkit-tab');
 await openSettings(page);await page.getByRole('dialog',{name:'Settings',exact:true}).waitFor();
 const geometry=await page.getByRole('dialog',{name:'Settings',exact:true}).boundingBox();assert.equal(geometry.width,800);assert.equal(geometry.height,800);
 await page.screenshot({path:join(report,'settings-independent.png')});await page.getByRole('button',{name:'Close settings',exact:true}).click();
 await contribution.locator('pre').filter({hasText:'Independent panel bytes'}).waitFor();checks.push('Settings preserves the resource presentation');
 await (await resourceGuide(page)).getByRole('button',{name:'Workspace files',exact:true}).click();
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===2);
 await page.getByRole('button',{name:'Split resources',exact:true}).first().click();
 const divider=page.getByRole('separator',{name:'Resize resource panes',exact:true});
 await divider.focus();await page.keyboard.press('ArrowRight');await attributeSettles(divider,'aria-valuenow','52');
 assert(await divider.evaluate(el=>el===document.activeElement));
 await page.keyboard.press('Home');await attributeSettles(divider,'aria-valuenow','20');
 await page.keyboard.press('End');await attributeSettles(divider,'aria-valuenow','80');
 const narrowGuide=page.locator('[data-dockkit-pane][data-dockkit-column="1"] [data-dockkit-tab][aria-selected=true]');
 await narrowGuide.hover();
 for(const target of [narrowGuide,narrowGuide.locator('[data-dockkit-tab-close]')]){
   assert(await target.evaluate(el=>{const rect=el.getBoundingClientRect();return el.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2))}),'narrow guide and its close control must receive pointer events');
 }
 assert(await page.locator('[data-dockkit-strip-chrome] button:enabled').evaluateAll(controls=>controls.every(el=>{
   const rect=el.getBoundingClientRect();return el.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2));
 })),'enabled dock actions remain clickable in a narrow pane');
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();await attributeSettles(divider,'aria-valuenow','20');
 await divider.focus();for(const value of ['30','40','50']){await page.keyboard.press('Shift+ArrowRight');await attributeSettles(divider,'aria-valuenow',value);}
 await page.screenshot({path:join(report,'split-keyboard.png')});checks.push('Split keyboard bounds, focus and product undo');
 const chips=page.locator('[data-dockkit-tab]');const last=chips.filter({hasText:'Files'}).last();await last.click({button:'right'});await page.getByRole('menuitem',{name:'Float',exact:true}).click();
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
 const beforeOpenRoster=await rosterCalls();
 await resourceGuide(page);
 if(process.platform==='linux'||process.platform==='win32') {
 await more.waitFor();assert.equal(await rosterCalls(),beforeOpenRoster);
 const beforeCreateRoster=await rosterCalls();
 await activeGuide.getByRole('button',{name:'New terminal',exact:true}).click();
 const terminal=page.getByRole('region',{name:'Session terminals',exact:true});
 await terminal.getByRole('status').filter({hasText:'You have control'}).waitFor();
 const input=terminal.locator('.xterm-helper-textarea');
 const terminalFocused=()=>input.evaluate(element=>element===document.activeElement);
 await waitUntil(terminalFocused,'creating a writable terminal focuses its keyboard input');
 await toggleResources.click();
 await terminal.waitFor({state:'hidden'});
 await toggleResources.click();
 await terminal.waitFor();
 await waitUntil(terminalFocused,'reopening the writable terminal restores keyboard focus');
 assert.equal(await rosterCalls(),beforeCreateRoster,'individual terminal creation does not load a hidden roster');
 assert.equal(await terminal.getByRole('button',{name:'Refresh',exact:true}).count(),0);
 if(process.platform==='win32')await terminal.getByText('Current user permissions',{exact:true}).waitFor();
 const shellInput=process.platform==='win32'
   ? "[IO.File]::WriteAllText((Join-Path $PWD 'dock-pty.txt'),'dock-terminal-ok'); Write-Host 'DOCK_TERMINAL_VISIBLE' -ForegroundColor Yellow"
   : "printf 'dock-terminal-ok' > dock-pty.txt; printf '\\033[93mDOCK_TERMINAL_VISIBLE\\033[0m\\n'";
 await page.keyboard.type(shellInput);await page.keyboard.press('Enter');
 const terminalAction=terminal.getByRole('button',{name:'New terminal',exact:true});
 await terminalAction.focus();
 await waitUntil(async()=>await readFile(join(service.workspace,'dock-pty.txt'),'utf8').catch(()=>null)==='dock-terminal-ok','real Session terminal input');
 const output=terminal.locator('.xterm-rows > div').filter({hasText:/^\s*DOCK_TERMINAL_VISIBLE\s*$/});
 await output.waitFor();
 const contrast=await output.evaluate(row=>{
   const foreground=getComputedStyle(row.querySelector('span')).color;
   const background=getComputedStyle(row.closest('.xterm').querySelector('.xterm-viewport')).backgroundColor;
   const luminance=color=>{
     const [r,g,b]=color.match(/[\d.]+/g).slice(0,3).map(Number).map(channel=>{
       const value=channel/255;
       return value<=.04045?value/12.92:((value+.055)/1.055)**2.4;
     });
     return .2126*r+.7152*g+.0722*b;
   };
   const a=luminance(foreground),b=luminance(background);
   const [red,green,blue]=foreground.match(/[\d.]+/g).map(Number);
   return {foreground,background,yellow:red>blue&&green>blue,ratio:(Math.max(a,b)+.05)/(Math.min(a,b)+.05)};
 });
 await writeFile(join(report,'terminal-contrast.json'),JSON.stringify(contrast,null,2));
 await page.screenshot({path:join(report,'terminal-contrast.png')});
 assert(contrast.ratio>=4.5,'highlighted shell output must reach 4.5 contrast: '+JSON.stringify(contrast));
 assert(contrast.yellow,'highlighted shell output must retain its yellow hue');
 assert(await terminalAction.evaluate(element=>element===document.activeElement),'terminal output preserves focus on other controls');
 checks.push('Creating or reopening a writable terminal focuses input; subsequent output preserves focus');
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
 assert.equal(await rosterCalls(),beforeCloseRoster);
 const beforeRestoreRoster=await rosterCalls();await page.getByRole('button',{name:'Undo layout',exact:true}).click();
 await terminal.getByRole('status').filter({hasText:'Read only'}).waitFor();
 assert.equal(await terminalFocused(),false,'a restored read-only terminal keeps focus on the user control');
 assert.equal(await rosterCalls(),beforeRestoreRoster,'reattachment does not load a hidden roster');
 checks.push('Individual terminal tabs keep listing and refresh in the launcher');
 assert.equal((await followerCalls()).filter(call=>call.type==='create').length,1,'undo attaches and never creates or takes control');
 checks.push('Real terminal follower survives float and dock; close detaches; undo attaches read only');
 await terminal.getByRole('button',{name:'Hide terminal panel',exact:true}).click();
 await terminal.waitFor({state:'hidden'});
 const beforeRoster=await page.locator('[data-dockkit-tab]').count();
 await resourceGuide(page);
 assert.equal(await page.locator('[data-dockkit-tab]').count(),beforeRoster,'reopening the retained roster uses the existing guide');
 await more.click();await listTerminals.click();await page.getByRole('menuitem',{name:/^Terminal 1/}).click();
 await terminal.getByRole('status').filter({hasText:'Read only'}).waitFor();
 assert.equal((await followerCalls()).filter(call=>call.type==='create').length,1);
 checks.push('Retained terminal roster refreshes when reopened and attaches its live shell');
 await terminal.getByRole('button',{name:'Take control',exact:true}).click();
 await terminal.getByRole('status').filter({hasText:'You have control'}).waitFor();
 await waitUntil(terminalFocused,'taking control focuses the terminal input');
 await page.evaluate(()=>window.holdTerminalResize=true);await page.setViewportSize({width:1024,height:768});
 await page.waitForFunction(()=>!!window.releaseTerminalResize);
 await page.keyboard.type('exit 0');await page.keyboard.press('Enter');
 await terminal.getByRole('status').filter({hasText:'Exited 0'}).waitFor();
 if(process.platform==='win32'){
   assert.match(await readFile(shellHistoryPath,'utf8'),/dock-terminal-ok/,'the real terminal records its command inside the disposable profile');
   checks.push('Windows PowerShell command history stays in the fixture profile');
 }
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
