import {build,preview} from '../../../apps/web/node_modules/vite/dist/node/index.js';
import {chromium} from '../web-product/node_modules/playwright/index.mjs';
import {mkdir,writeFile,readFile,copyFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
const reportIndex=process.argv.indexOf('--report');assert(reportIndex>=0);
const report=resolve(process.argv[reportIndex+1]),capture=process.argv.includes('--capture-candidates');
await mkdir(report,{recursive:false});
const web=resolve('apps/web'),fixture=resolve('fixtures/rsi/workbench-reference'),root=join(report,'scene');await mkdir(root);
await copyFile(join(fixture,'settings-scene.tsx'),join(root,'scene.tsx'));
await writeFile(join(root,'index.html'),'<html><head><meta charset="utf-8"></head><body><div id="root"></div><script type="module" src="/scene.tsx"></script></body></html>');
const config={configFile:false,root,logLevel:'warn',esbuild:{jsx:'automatic'},resolve:{alias:[{find:'@fixture/web',replacement:web},...['react-dom','react','clsx'].map(name=>({find:name,replacement:join(web,'node_modules',name)}))]},build:{outDir:join(root,'dist'),emptyOutDir:true}};
await build(config);const server=await preview({...config,preview:{host:'127.0.0.1',port:0}}),browser=await chromium.launch();const scenes=[];
try{
 for(const [width,height]of [[1440,900],[1024,768],[767,900],[390,844]])for(const theme of ['light','dark']){
  const context=await browser.newContext({viewport:{width,height},deviceScaleFactor:1,colorScheme:theme,reducedMotion:'reduce'}),page=await context.newPage(),errors=[];
  page.on('pageerror',e=>errors.push(e.message));await page.goto(server.resolvedUrls.local[0]+`?theme=${theme}`);
  await page.getByRole('button',{name:'Plugins',exact:true}).click();await page.getByLabel('SSH MCP server name',{exact:true}).waitFor();await page.mouse.move(0,0);
  const geometry=await page.evaluate(()=>{const modal=document.querySelector('.settings-modal'),field=document.querySelector('[aria-label="SSH MCP server name"]'),options=document.querySelector('.settings-options');return {modal:modal.getBoundingClientRect().toJSON(),overflow:document.documentElement.scrollWidth-innerWidth,optionsOverflow:options.scrollWidth-options.clientWidth,field:field.getBoundingClientRect().toJSON()}});
  assert(geometry.overflow<=1&&geometry.optionsOverflow<=1,JSON.stringify(geometry));assert.deepEqual(errors,[]);
  const name=`ssh-settings-${width}-${theme}.png`,path=join(report,name);await page.screenshot({path});
  const hash=createHash('sha256').update(await readFile(path)).digest('hex');let comparison;
  if(!capture){
   const urls=await Promise.all([join(fixture,'goldens',name),path].map(async path=>'data:image/png;base64,'+(await readFile(path)).toString('base64')));
   comparison=await page.evaluate(async urls=>{const images=await Promise.all(urls.map(async src=>{const i=new Image();i.src=src;await i.decode();return i}));const [a,b]=images;if(a.width!==b.width||a.height!==b.height)throw Error('Golden dimensions differ');const c=document.createElement('canvas');c.width=a.width;c.height=a.height;const x=c.getContext('2d',{willReadFrequently:true}),pixels=images.map(i=>{x.drawImage(i,0,0);return x.getImageData(0,0,c.width,c.height).data});let different=0;for(let i=0;i<pixels[0].length;i+=4)if([0,1,2].some(k=>Math.abs(pixels[0][i+k]-pixels[1][i+k])>16))different++;return {different,fraction:different/(c.width*c.height)}},urls);
   assert(comparison.fraction<=.01,JSON.stringify({name,...comparison}));
  }
  scenes.push({name,width,height,theme,sha256:hash,geometry,comparison});await context.close();
 }
 await writeFile(join(report,'result.json'),JSON.stringify({browser:browser.version(),renderingOnly:true,candidates:capture,mask:'none',channelThreshold:16,maximumDifferentPixels:.01,scenes},null,2));
}finally{await browser.close();await new Promise(resolve=>server.httpServer.close(resolve))}
