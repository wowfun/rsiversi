// Closed-event presentation, real DOM and bounded Shiki; no Service or credentials.
import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';
import {chromium,firefox} from 'playwright';
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
setClipboardWriter(async text=>{window.copied=text});
const root=createRoot(article);window.render=(streaming=false)=>flushSync(()=>root.render(React.createElement(React.Fragment,null,React.createElement(IconSprite),React.createElement(Markdown,{events:nodes,streaming}))));window.render();
window.ready=true;
`;
const server=await sourceFixture({name:'markdown-scene',source:script});const results=[];
try{
  for(const [name,engine] of Object.entries({chromium,firefox})){
    const browser=await engine.launch();
    try{
      const page=await browser.newPage(),errors=[];page.on('pageerror',error=>{errors.push(error.message);console.error(name,error.message)});
      await page.goto(server.url);await page.waitForFunction(()=>window.ready);
      assert.equal(await page.locator('article img,article script').count(),0);
      assert.equal(await page.locator('th[scope=col]').count(),2);
      assert.equal(await page.locator('td[data-align=right]').evaluate(node=>getComputedStyle(node).textAlign),'right');
      assert.equal(await page.locator('pre code').textContent(),await page.evaluate(()=>window.code));
      await page.locator('.code-token[style]').first().waitFor();
      await page.evaluate(()=>window.render(true));
      assert.equal(await page.locator('.code-token').count(),0,'Streaming source stays plain');
      assert.equal(await page.locator('pre code').textContent(),await page.evaluate(()=>window.code));
      await page.evaluate(()=>window.render(false));
      await page.locator('.code-token').first().waitFor();
      await page.getByRole('button',{name:'Copy code'}).click();assert.equal(await page.evaluate(()=>window.copied),await page.evaluate(()=>window.code));
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
      assert.notEqual(colors[0],colors[2]);assert.deepEqual(errors,[]);
      results.push({browser:name,version:browser.version(),literalMarkup:true,headers:true,alignment:true,exactCopy:true,wrap:true,themeColors:colors,errors});
    }finally{await browser.close()}
  }
  await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
}finally{await server.close()}
