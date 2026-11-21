// Closed-event presentation, real DOM and bounded Shiki; no Service or credentials.
import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';
import {chromium,firefox} from 'playwright';
import {png} from './png.mjs';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const script=`
import React from 'react';import {createRoot} from 'react-dom/client';import {flushSync} from 'react-dom';
import {IconSprite} from '/src/icon-sprite.tsx';
import {Markdown} from '/src/session/markdown.tsx';import {setClipboardWriter} from '/mounts.js';
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
const root=createRoot(article);
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
      const imageLink=page.getByRole('link',{name:'Diagram',exact:true});
      await imageLink.scrollIntoViewIfNeeded();
      await page.waitForFunction(()=>document.querySelector('article a img')?.naturalWidth===24);
      assert.equal(await imageLink.getAttribute('href'),'https://media.example.test/original');
      assert.equal(await imageLink.locator('button').count(),0);
      assert.equal(imageHeaders.referer,undefined);
      assert.deepEqual(errors,[]);
      results.push({browser:name,version:browser.version(),literalMarkup:true,headers:true,alignment:true,exactCopy:true,wrap:true,themeColors:colors,ordinaryMathLoads:0,streamingMathLoads:0,annotations,copiedTex,layout,wideTable,linkedImage:true,errors});
    }finally{await browser.close()}
  }
  await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
}finally{await server.close()}
