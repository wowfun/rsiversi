import {comparePixels} from './pixels.mjs';
import {build,preview} from '../../../apps/web/node_modules/vite/dist/node/index.js';
import {chromium} from '../web-product/node_modules/playwright/index.mjs';
import {mkdir,writeFile,readFile,copyFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
const report=resolve(process.argv[process.argv.indexOf('--report')+1]);await mkdir(report,{recursive:false});
const repository=resolve(process.env.RSI_DSH_REFERENCE ?? '.references/rsi/deepseek-harness'),revision=execFileSync('git',['-C',repository,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
assert.equal(revision,'4878cdabd87d4041bdaff61d04c966883b9fd07a');
const reference=join(repository,'packages/client'),web=resolve('apps/web'),fixture=resolve('fixtures/rsi/workbench-reference');
const primitives=join(reference,'ui-primitives/src');
const sources={};
for(const name of ['scene.tsx','scene.css'])sources[name]=createHash('sha256').update(await readFile(join(fixture,name))).digest('hex');
const browser=await chromium.launch(),results={revision,browser:browser.version(),sources,scenes:[]};
try{
 for(const variant of ['reference','rsi']){
  const root=join(report,variant);await mkdir(root);
  await copyFile(join(fixture,'scene.tsx'),join(root,'scene.tsx'));await copyFile(join(fixture,'scene.css'),join(root,'scene.css'));
  await writeFile(join(root,'index.html'),'<html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module" src="/scene.tsx"></script></body></html>');
  await writeFile(join(root,'primitives.ts'),['Tooltip','MenuSurface','focus','useModalLayer','keyboard-composition','icons/index'].map(name=>`export * from ${JSON.stringify(join(primitives,name+(name.includes('icons')||['Tooltip','MenuSurface'].includes(name)?'.tsx':'.ts')))};`).join('\n'));
  await writeFile(join(root,'theme.css'),variant==='reference'?
   ['base','design-platform','gradient-shadow-text'].map(name=>`@import ${JSON.stringify(join(reference,`ui-theme/src/styles/${name}.css`))};`).join('\n'):
   `@import ${JSON.stringify(join(web,'styles.css'))};\n@import ${JSON.stringify(join(web,'src/workbench.css'))};\n@import ${JSON.stringify(join(web,'vendor/dsh/dockkit-tokens.css'))};`);
  const config={configFile:false,root,logLevel:'warn',esbuild:{jsx:'automatic'},resolve:{alias:[
   {find:'@fixture/dock',replacement:variant==='reference'?join(reference,'ui-dockkit/src'):join(web,'vendor/dsh/dockkit')},
   {find:'@fixture/theme',replacement:join(root,'theme.css')},{find:'@deepseek-ai/dsh-client-ui-primitives',replacement:join(root,'primitives.ts')},
   ...['react-dom','react','clsx'].map(name=>({find:name,replacement:join(web,'node_modules',name)}))]},build:{outDir:join(root,'dist'),emptyOutDir:true}};
  await build(config);const server=await preview({...config,preview:{host:'127.0.0.1',port:0}});
  try{
   for(const [width,height] of [[1440,900],[1024,768]])for(const theme of ['light','dark']){
    const context=await browser.newContext({viewport:{width,height},deviceScaleFactor:1,colorScheme:theme,reducedMotion:'reduce'}),page=await context.newPage();
    const errors=[];page.on('pageerror',error=>errors.push(error.message));
    await page.goto(server.resolvedUrls.local[0]+`?theme=${theme}`);try{await page.locator('[data-dockkit-float]').waitFor({timeout:5000})}catch(error){await writeFile(join(report,`${variant}-failure.json`),JSON.stringify({errors,html:await page.content()},null,2));throw error}
    const measured=await page.evaluate(()=>[...document.querySelectorAll('[data-dockkit-pane],[data-dockkit-float],[data-dockkit-tab]')].map(node=>{const b=node.getBoundingClientRect(),s=getComputedStyle(node);return {kind:node.hasAttribute('data-dockkit-tab')?'tab':node.hasAttribute('data-dockkit-float')?'float':'pane',rect:[b.x,b.y,b.width,b.height],tokens:{color:s.color,background:s.backgroundColor,radius:s.borderRadius,font:s.font,fontSize:s.fontSize,shadow:s.boxShadow,border:s.border}}}));
    assert.deepEqual(errors,[]);await page.mouse.move(0,0);await page.screenshot({path:join(report,`${variant}-${width}-${theme}.png`)});
    results.scenes.push({variant,width,height,theme,measured});await context.close();
   }
  }finally{await new Promise(resolve=>server.httpServer.close(resolve))}
 }
 await writeFile(join(report,'geometry.json'),JSON.stringify(results,null,2));
 for(const actual of results.scenes.filter(scene=>scene.variant==='rsi')){
  const reference=results.scenes.find(scene=>scene.variant==='reference'&&scene.width===actual.width&&scene.theme===actual.theme);
  assert.equal(actual.measured.length,reference.measured.length);
  actual.measured.forEach((item,index)=>{const expected=reference.measured[index];assert.equal(item.kind,expected.kind);assert.deepEqual(item.tokens,expected.tokens);item.rect.forEach((n,i)=>assert(Math.abs(n-expected.rect[i])<=2));});
 }
 await comparePixels(browser,report);
 await writeFile(join(report,'result.json'),JSON.stringify({revision,browser:browser.version(),component:'dockkit with fixed content',geometryTolerance:2,channelThreshold:16,maximumDifferentPixels:.01,mask:'none',passed:true},null,2));
}finally{await browser.close()}
