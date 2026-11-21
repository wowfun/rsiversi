// Closed-event presentation, real DOM and bounded Shiki; no Service or credentials.
import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';
import {chromium,firefox} from 'playwright';
import {png,unreadablePng} from './png.mjs';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const script=`
import React from 'react';import {createRoot} from 'react-dom/client';import {flushSync} from 'react-dom';
import {IconSprite} from '/src/icon-sprite.tsx';
import {Markdown} from '/src/session/markdown.tsx';import {setClipboardWriter} from '/mounts.js';
import {DocumentSessionScope} from '/src/document-plugins.tsx';import {installActions} from '/src/bridge.ts';
import {bindImageIntake} from '/src/session/image-intake.ts';
import '/styles.css';import '/vendor/dsh/theme-base.css';import session from '/src/session/surface.module.css';import '/vendor/dsh/dockkit-tokens.css';import '/code-highlight.css';
const start=(kind,fields={})=>({kind:'start',element:{kind,...fields}}),end={kind:'end'},text=value=>({kind:'text',text:value});
window.code='const value = "<img src=x onerror=alert(1)>";\\nconsole.log("MARKDOWN_OK");\\n';
const nodes=[start('paragraph'),text('A closed Markdown event stream keeps literal text, table alignment and the exact code source.'),end,start('table'),start('table_head'),start('table_cell',{header:true,alignment:'left'}),text('Feature'),end,start('table_cell',{header:true,alignment:'right'}),text('Result'),end,end,start('table_row'),start('table_cell',{header:false,alignment:'left'}),text('<script>literal</script>'),end,start('table_cell',{header:false,alignment:'right'}),text('Preserved'),end,end,end,start('pre',{language:'js'}),text(window.code),end];
window.copied=null;window.actions=[];
const article=document.createElement('article');article.className='message-text markdown '+session.scope;article.style.cssText='padding:24px;max-width:760px;margin:auto';document.body.append(article);
window.copyWrites=0;
setClipboardWriter(async text=>{window.copyWrites++;window.copied=text;await new Promise(resolve=>{window.finishCopy=resolve})});
const formula=(text,display=false)=>({kind:'math',text,display});
const formulas=[start('paragraph'),text('勾股定理 '),formula('x^2'),end,
  formula('\\\\frac{1}{2} + \\\\int_0^1 t^2 dt',true),
  start('pre',{language:'math'}),text('a+b=c'),end,
  formula('\\\\frac{1}{'),formula('a+'.repeat(80)+'a',true)];
const table=[start('table'),start('table_head'),...['城市','结果','日期','颜色','数量','类别','单位','项目'].flatMap(label=>[start('table_cell',{header:true}),text(label),end]),end,
  start('table_row'),...['上海','ABCDEFGHIJ'.repeat(13),'周一','橙','19','花','朵','果园'].flatMap(value=>[start('table_cell'),text(value),end]),end,end];
const tables=[...table,start('quote'),...table,end];
const linkedImage=[start('paragraph'),start('link',{href:'https://media.example.test/original'}),
  start('image',{src:'https://media.example.test/diagram.png'}),text('Diagram'),end,end,end];
const localImage=[start('paragraph'),start('file_image',{path:'../diagram.svg'}),text('Local diagram'),end,end];
const brokenImage=[start('paragraph'),start('file_image',{path:'../unreadable.png'}),end,end];
window.reads=[];window.revoked=[];
const revoke=URL.revokeObjectURL.bind(URL);
URL.revokeObjectURL=url=>{window.revoked.push(url);revoke(url)};
installActions({call:async(method,raw)=>{
  window.reads.push([method,JSON.parse(raw)]);
  if(JSON.parse(raw).path==='../unreadable.png')return ['image/png',Uint8Array.from(window.broken)];
  return ['image/svg+xml',new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg" width="96" height="48"><rect width="96" height="48" fill="teal"/></svg>')];
}});
window.mountImageDrop=()=>{
  const form=document.createElement('form'),input=document.createElement('textarea');
  form.style.cssText='position:fixed;bottom:20px;right:20px';
  form.append(input);document.body.append(form);
  const outside=document.createElement('div');outside.id='outside-composer';
  outside.style.cssText='position:fixed;top:40px;right:20px;width:80px;height:80px';document.body.append(outside);
  window.dropped=[];window.dropOverlay=[];
  window.releaseDrop=bindImageIntake(input,{index:'main',composerView:{mediaLimits:{}},
    upload:async files=>{for(const file of files)window.dropped.push({name:file.name,type:file.type,text:await file.text()});}},active=>window.dropOverlay.push(active));
};
const root=createRoot(article);
window.renderLocal=(streaming,generation,broken=false)=>{const view={session:'document',generation};
  flushSync(()=>root.render(React.createElement(DocumentSessionScope,{view,owner:{index:'main',staged:{data:view}}},
    React.createElement(Markdown,{events:broken?brokenImage:localImage,streaming}))))};
window.render=(streaming=false,math=false)=>flushSync(()=>root.render(React.createElement(React.Fragment,null,
  React.createElement(IconSprite),React.createElement(Markdown,{events:math==='image'?linkedImage:math==='tables'?tables:math?formulas:nodes,streaming}))));
window.render();
window.ready=true;
`;
const server=await sourceFixture({name:'markdown-scene',source:script});const results=[];
try{
  for(const [name,engine] of Object.entries({chromium,firefox})){
    const browser=await engine.launch();
    try{
      const page=await browser.newPage(),errors=[];page.on('pageerror',error=>{errors.push(error.message);console.error(name,error.message)});
      await page.clock.install({time:new Date('2026-01-01T00:00:00Z')});
      await page.clock.pauseAt(new Date('2026-01-01T00:01:00Z'));
      await page.goto(server.url);
      await page.clock.runFor(100);
      assert(await page.evaluate(()=>window.ready));
      assert.equal(await page.locator('article img,article script').count(),0);
      assert.equal(await page.locator('th[scope=col]').count(),2);
      assert.equal(await page.locator('td[data-align=right]').evaluate(node=>getComputedStyle(node).textAlign),'right');
      assert.equal(await page.locator('pre code').textContent(),await page.evaluate(()=>window.code));
      await page.locator('.code-token[style]').first().waitFor();
      await page.clock.resume();
      await page.evaluate(()=>window.render(true));
      assert.equal(await page.locator('.code-token').count(),0,'Streaming source stays plain');
      assert.equal(await page.locator('pre code').textContent(),await page.evaluate(()=>window.code));
      await page.evaluate(()=>window.render(false));
      await page.locator('.code-token').first().waitFor();
      const copy=page.getByRole('button',{name:'Copy code'});
      await copy.click();await copy.press('Enter');
      assert.equal(await page.evaluate(()=>window.copyWrites),1,'a pending write accepts one activation');
      assert.equal(await page.evaluate(()=>window.copied),await page.evaluate(()=>window.code));
      await page.evaluate(()=>window.finishCopy());
      await page.getByRole('button',{name:'Copied',exact:true}).press('Enter');
      assert.equal(await page.evaluate(()=>window.copyWrites),1,'success feedback protects the accepted write');
      await copy.waitFor({timeout:3000});await copy.press('Enter');
      assert.equal(await page.evaluate(()=>window.copyWrites),2);
      await page.evaluate(()=>window.finishCopy());
      assert.equal(await page.locator('pre').evaluate(node=>getComputedStyle(node).whiteSpace),'pre-wrap');
      await page.getByRole('button',{name:'Wrap lines'}).click();assert.equal(await page.locator('pre').evaluate(node=>getComputedStyle(node).whiteSpace),'pre');
      await page.getByRole('button',{name:'Wrap lines'}).click();assert.equal(await page.locator('pre').evaluate(node=>getComputedStyle(node).whiteSpace),'pre-wrap');
      const colors=[];
      for(const theme of ['light','dark'])for(const width of [1440,390]){
        await page.setViewportSize({width,height:844});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
        const geometry=await page.evaluate(()=>({overflow:document.documentElement.scrollWidth>innerWidth,codeColor:getComputedStyle(document.querySelector('.code-token')).color}));
        assert.equal(geometry.overflow,false);colors.push(geometry.codeColor);
        await page.screenshot({path:join(report,`${name}-${theme}-${width}.png`)});
      }
      assert.notEqual(colors[0],colors[2]);
      const mathLoads=()=>page.evaluate(()=>performance.getEntriesByType('resource').filter(e=>new URL(e.name).pathname==='/math.js').length);
      assert.equal(await mathLoads(),0);
      await page.evaluate(()=>window.render(true,true));
      assert((await page.locator('article').innerText()).includes('$x^2$'));
      assert.equal(await page.locator('.katex').count(),0);assert.equal(await mathLoads(),0);
      await page.evaluate(()=>window.render(false,true));
      await page.locator('.katex').nth(3).waitFor();
      await page.evaluate(()=>document.fonts.ready);
      const annotations=await page.locator('.katex annotation').allTextContents();
      assert.deepEqual(annotations,['x^2','\\frac{1}{2} + \\int_0^1 t^2 dt','a+b=c','a+'.repeat(80)+'a']);
      assert.equal(await page.locator('.katex-error').innerText(),'\\frac{1}{');
      const copiedTex=await page.locator('.katex-html').first().evaluate(node=>{
        const selection=document.getSelection(),range=document.createRange();
        range.selectNodeContents(node);selection.removeAllRanges();selection.addRange(range);
        const clipboardData=new DataTransfer();
        const event=new ClipboardEvent('copy',{bubbles:true,cancelable:true,clipboardData});
        node.dispatchEvent(event);selection.removeAllRanges();
        return {text:event.clipboardData.getData('text/plain'),html:event.clipboardData.getData('text/html'),handled:event.defaultPrevented};
      });
      assert.deepEqual(copiedTex,{text:'$x^2$',html:'$x^2$',handled:true});
      const layout=await page.locator('.katex-display').last().evaluate(node=>({
        innerScrolling:node.scrollWidth>node.clientWidth,documentWidth:document.documentElement.scrollWidth,
        font:document.fonts.check('16px KaTeX_Main'),viewport:innerWidth}));
      assert(layout.innerScrolling && layout.font);assert.equal(layout.documentWidth,layout.viewport);
      await page.screenshot({path:join(report,name+'-math-narrow.png')});
      await page.setViewportSize({width:1440,height:844});
      await page.evaluate(()=>document.documentElement.dataset.theme='light');
      await page.screenshot({path:join(report,name+'-math-wide.png')});
      await page.evaluate(()=>window.render(false,'tables'));
      await page.setViewportSize({width:420,height:844});await page.mouse.move(0,0);
      const wrapper=page.locator('.markdown-table').first(),quote=page.locator('blockquote .markdown-table');
      assert.equal(await wrapper.getAttribute('tabindex'),'0');assert.equal(await quote.getAttribute('tabindex'),null);
      const overflow=()=>wrapper.evaluate(el=>getComputedStyle(el).overflowX);
      assert.equal(await overflow(),'hidden');await wrapper.getByRole('columnheader').first().hover();assert.equal(await overflow(),'scroll');
      await page.mouse.move(0,0);await page.keyboard.press('Tab');
      await wrapper.evaluate(el=>el.focus());assert.equal(await overflow(),'scroll');
      const wideTable=await wrapper.evaluate(el=>{el.scrollLeft=70;return {width:el.clientWidth,content:el.scrollWidth,offset:el.scrollLeft,viewport:innerWidth,documentWidth:document.documentElement.scrollWidth};});
      assert(wideTable.offset>0);assert.equal(wideTable.viewport,wideTable.documentWidth);
      await page.screenshot({path:join(report,name+'-wide-table.png')});
      let imageHeaders;
      await page.route('https://media.example.test/diagram.png',async route=>{
        imageHeaders=await route.request().allHeaders();
        await route.fulfill({contentType:'image/png',body:png(24,16,[32,120,145,255])});
      });
      await page.evaluate(()=>window.render(false,'image'));
      const imageLink=page.locator('article a[href="https://media.example.test/original"]');
      await page.locator('article').evaluate(el=>el.scrollIntoView({block:'start'}));
      await page.waitForFunction(()=>document.querySelector('article a img')?.naturalWidth===24);
      assert.equal(await page.getByRole('link',{name:'Diagram',exact:true}).count(),1);
      assert.equal(await imageLink.getAttribute('href'),'https://media.example.test/original');
      assert.equal(await imageLink.locator('button').count(),0);
      assert.equal(imageHeaders.referer,undefined);
      await page.evaluate(()=>window.renderLocal(true,'1'));
      assert.equal(await page.locator('article').innerText(),'Local diagram');
      assert.deepEqual(await page.evaluate(()=>window.reads),[]);
      await page.evaluate(()=>window.renderLocal(false,'1'));
      const local=page.getByRole('button',{name:'View full image: Local diagram'}).getByRole('img');
      await local.evaluate(element=>element.decode());
      const firstUrl=await local.getAttribute('src');
      assert(firstUrl.startsWith('blob:'));
      await page.evaluate(()=>window.renderLocal(false,'2'));
      await local.evaluate(element=>element.decode());
      assert.notEqual(await local.getAttribute('src'),firstUrl);
      assert.deepEqual(await local.evaluate(el=>[el.naturalWidth,el.naturalHeight]),[96,48]);
      await local.scrollIntoViewIfNeeded();
      await page.screenshot({path:join(report,name+'-local-image.png')});
      await page.evaluate(()=>window.mountImageDrop());
      const editor=page.getByRole('textbox');
      await local.dragTo(page.locator('#outside-composer'));
      assert.deepEqual(await page.evaluate(()=>window.dropped),[]);
      await local.dragTo(editor);
      await page.waitForFunction(()=>window.dropped.length===1);
      const [dropped]=await page.evaluate(()=>window.dropped);
      assert.equal(dropped.name,'diagram.svg');assert.equal(dropped.type,'image/svg+xml');
      assert.equal(dropped.text,'<svg xmlns="http://www.w3.org/2000/svg" width="96" height="48"><rect width="96" height="48" fill="teal"/></svg>');
      assert.equal(await editor.inputValue(),'');assert.equal(await page.getByRole('dialog').count(),0);
      assert(!await page.evaluate(()=>window.dropOverlay.includes(true)));
      await page.evaluate(()=>window.releaseDrop());
      await page.evaluate(()=>window.render());
      assert.deepEqual(await page.evaluate(()=>window.reads),['1','2'].map(generation=>['transcript_image',{pane:'main',generation,path:'../diagram.svg'}]));
      assert.equal(await page.evaluate(()=>new Set(window.revoked).size),2);
      await page.evaluate(bytes=>{window.broken=bytes;window.renderLocal(false,'3',true);},Array.from(unreadablePng()));
      const failure=page.getByText(/^Image preview unavailable · /);
      await failure.waitFor();
      const fallback=await failure.innerText();
      const releasedAfterFailure=await page.evaluate(()=>new Set(window.revoked).size);
      await page.screenshot({path:join(report,name+'-decode-failure.png')});
      await writeFile(join(report,name+'-decode-failure.json'),JSON.stringify({fallback,releasedAfterFailure}));
      assert.equal(fallback,'Image preview unavailable · ../unreadable.png');
      assert.equal(releasedAfterFailure,3,'failed local image is released while its message stays open');
      assert.deepEqual(errors,[]);
      results.push({browser:name,version:browser.version(),literalMarkup:true,headers:true,alignment:true,exactCopy:true,wrap:true,themeColors:colors,ordinaryMathLoads:0,streamingMathLoads:0,annotations,copiedTex,layout,wideTable,linkedImage:true,
        localImage:{readCount:await page.evaluate(()=>window.reads.length),originalName:dropped.name,originalType:dropped.type,
          releasedUrls:await page.evaluate(()=>new Set(window.revoked).size),droppedFiles:await page.evaluate(()=>window.dropped.length)},errors});
    }finally{await browser.close()}
  }
  await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
}finally{await server.close()}
