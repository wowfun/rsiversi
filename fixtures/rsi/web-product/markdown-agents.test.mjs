import assert from 'node:assert/strict';
import {test} from 'node:test';
import {chromium,firefox} from 'playwright';
import {sendMarkdownTask} from './markdown-agents.mjs';
import {waitUntil} from './service.mjs';

for(const [name,engine] of Object.entries({chromium,firefox})) {
 test(`${name}: Markdown task does not use a busy sending choice`,async()=>{
  const browser=await engine.launch();
  try {
   const page=await browser.newPage();
   const bodies=[];
   await page.exposeFunction('providerRequest',()=>bodies.push({messages:[{role:'user',content:'unrelated completion'}]}));
   await page.setContent('<section><textarea aria-label="Main message"></textarea><button data-testid="composer-send" aria-label="Queue">Queue</button><p class="pane-status">Completed</p></section>');
   await page.evaluate(()=>{window.clicks=0;document.querySelector('button').onclick=()=>{window.clicks++;window.providerRequest()}});
   page.setDefaultTimeout(1000);
   await assert.rejects(sendMarkdownTask(page.locator('section'),'second task',bodies),/Timeout/);
   assert.equal(await page.evaluate(()=>window.clicks),0);
  }finally{await browser.close()}
 });
 test(`${name}: unrelated provider activity cannot acknowledge a Markdown task`,async()=>{
  const browser=await engine.launch();
  try {
   const page=await browser.newPage(),bodies=[];
   await page.exposeFunction('providerRequest',()=>bodies.push({messages:[{role:'user',content:'unrelated completion'}]}));
   await page.setContent('<section><textarea aria-label="Main message"></textarea><button data-testid="composer-send" aria-label="Send">Send</button><p class="pane-status">Completed</p></section>');
   await page.evaluate(()=>{window.clicks=0;document.querySelector('button').onclick=()=>{window.clicks++;window.providerRequest()}});
   const sending=sendMarkdownTask(page.locator('section'),'second task',bodies);
   await page.waitForFunction(()=>window.clicks===1);
   await waitUntil(()=>bodies.length===1,'unrelated provider request');
   let timer;
   try {
    assert.equal(await Promise.race([sending.then(()=>true),new Promise(resolve=>{timer=setTimeout(()=>resolve(false),150)})]),false);
   }finally{clearTimeout(timer)}
   bodies.push({messages:[{role:'user',content:[{type:'text',text:'second task'}]}]});
   await sending;
   assert.equal(await page.evaluate(()=>window.clicks),1);
  }finally{await browser.close()}
 });
}
