import assert from "node:assert/strict";
import { test } from "node:test";
import { chromium, firefox } from "playwright";
import { browserNames, assertControls, assertNoNotices, recordTaskFailure } from "./task-checks.mjs";
import { clickUiControl } from './controls.mjs';
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
  test(`${name}: unchanged Workflow controls survive refresh and current dispatch invokes once`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();await page.setContent('<main></main>');
      await page.evaluate(async source=>{
        const url=URL.createObjectURL(new Blob([source],{type:'text/javascript'}));
        const {mount}=await import(url);URL.revokeObjectURL(url);
        let revision=0;window.invocations=[];
        const snapshot=(busy,value=1)=>({model:{standard_view:{elements:[
          {kind:'text',text:`revision ${++revision}`},
          {kind:'button',label:'Open workflow',action:'workflow',value}
        ]}},busy});
        const renderer=await mount(document.querySelector('main'),snapshot(false),{
          invoke(_action,input){window.invocations.push(input.value);}
        },new AbortController().signal);
        window.readyControl=()=>renderer.update(snapshot(false));
        window.pendingControl=()=>renderer.update(snapshot(true));
        window.changeControl=()=>renderer.update(snapshot(false,2));
        window.originalButton=document.querySelector('button');window.originalLabel=window.originalButton.firstChild;
        document.addEventListener('mousedown',()=>window.readyControl(),{once:true});
      },await readFile(new URL('../../../apps/web/standard.js',import.meta.url),'utf8'));
      await page.getByRole('button',{name:'Open workflow',exact:true}).click();
      assert.deepEqual(await page.evaluate(()=>window.invocations),[1], 'an unchanged control must receive its mouse gesture across a field refresh');
      assert(await page.evaluate(()=>document.querySelector('button')===window.originalButton&&document.querySelector('button').firstChild===window.originalLabel));
      await page.evaluate(()=>window.pendingControl());
      const pending=clickUiControl(page,'main','Open workflow',{timeout:2000});
      await page.evaluate(()=>window.readyControl());
      await pending;
      assert.deepEqual(await page.evaluate(()=>window.invocations),[1,1], 'the ready control is invoked once');
      await page.evaluate(()=>{window.changeControl();window.originalButton.click();});
      assert(await page.evaluate(()=>document.querySelector('button')!==window.originalButton));
      assert.deepEqual(await page.evaluate(()=>window.invocations),[1,1], 'retired action descriptors cannot dispatch');
      await clickUiControl(page,'main','Open workflow',{timeout:2000});
      assert.deepEqual(await page.evaluate(()=>window.invocations),[1,1,2]);
    } finally {await browser.close();}
  });
  test(`${name}: unavailable Workflow controls time out without dispatch or replay`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();
      for(const markup of ['<button disabled>Cancel workflow</button>','<button hidden>Cancel workflow</button>',
        '<button>Cancel workflow</button><div style="position:fixed;inset:0"></div>']) {
        await page.setContent(`<main>${markup}</main>`);
        await page.evaluate(()=>{window.invocations=0;document.querySelector('button').onclick=()=>++window.invocations;});
        await assert.rejects(clickUiControl(page,'main','Cancel workflow',{timeout:100}),/Timeout/);
        await page.evaluate(async()=>{
          const button=document.querySelector('button');button.disabled=false;button.hidden=false;
          document.querySelector('main>div')?.remove();
          await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
        });
        assert.equal(await page.evaluate(()=>window.invocations),0);
      }
    } finally {await browser.close();}
  });
  test(`${name}: settling an older invocation cannot enable the latest busy control`, async () => {
    const browser=await engine.launch({headless:true});
    try {
      const page=await browser.newPage();await page.setContent('<main></main>');
      await page.evaluate(async source=>{
        const url=URL.createObjectURL(new Blob([source],{type:'text/javascript'}));
        const {mount}=await import(url);URL.revokeObjectURL(url);
        const snapshot=busy=>({model:{standard_view:{elements:[{kind:'button',label:'Open workflow',action:'workflow',value:1}]}},busy});
        window.pending=new Promise(resolve=>{window.settle=resolve;});
        const renderer=await mount(document.querySelector('main'),snapshot(false),{invoke(){return window.pending;}},new AbortController().signal);
        window.makeBusy=()=>renderer.update(snapshot(true));
      },await readFile(new URL('../../../apps/web/standard.js',import.meta.url),'utf8'));
      await clickUiControl(page,'main','Open workflow',{timeout:2000});
      await page.evaluate(()=>window.makeBusy());
      await page.evaluate(async()=>{window.settle();await window.pending;});
      assert.equal(await page.getByRole('button',{name:'Open workflow',exact:true}).isEnabled(),false);
    } finally {await browser.close();}
  });
  test(`${name}: control capture waits for asynchronously mounted controls`, async () => {
    const browser = await engine.launch({ headless: true });
    try {
      const page = await browser.newPage();page.setDefaultTimeout(2000);
      await page.setContent("<main></main>");
      // Begin observation before the asynchronous renderer supplies the control.
      const pending = assertControls(page, "main", ["Complete result"]).then(value => ({value}), error => ({error}));
      await page.evaluate(() => document.querySelector("main").insertAdjacentHTML("beforeend", '<button>Complete result</button>'));
      const result = await pending;
      assert.ifError(result.error);
      assert.deepEqual(result.value, [{label:"Complete result",hit:true}]);
    } finally { await browser.close(); }
  });
  test(`${name}: control capture measures the current standard renderer after replacement`, async () => {
    const browser = await engine.launch({ headless: true });
    try {
      const page = await browser.newPage();
      await page.setContent("<main></main>");
      await page.evaluate(async source => {
        const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
        const { mount } = await import(url);
        URL.revokeObjectURL(url);
        let revision = 0;
        window.invocations = 0;
        const snapshot = () => ({ model: { standard_view: { elements: [{ kind: "button", label: "Pause after current round", action: "goal", value: ++revision }] } }, busy: false });
        const renderer = await mount(document.querySelector("main"), snapshot(), { invoke() { ++window.invocations; } }, new AbortController().signal);
        window.replaceControl = () => renderer.update(snapshot());
      }, await readFile(new URL("../../../apps/web/standard.js", import.meta.url), "utf8"));
      let replacements = 0;
      const replace = async () => {
        ++replacements;
        await page.evaluate(() => window.replaceControl());
      };
      // Preserve the real lookup/evaluation boundary: a locator can hand its
      // callback an element that the renderer retired after it was resolved.
      const wrapLocator = locator => new Proxy(locator, { get(target, property) {
        if (property === "getByRole") return (...args) => wrapLocator(target.getByRole(...args));
        if (property === "evaluate") return async (callback, arg) => {
          const element = await target.elementHandle();
          try { await replace(); return await element.evaluate(callback, arg); }
          finally { await element.dispose(); }
        };
        const value = Reflect.get(target, property);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      const observedPage = new Proxy(page, { get(target, property) {
        if (property === "locator") return (...args) => wrapLocator(target.locator(...args));
        if (property === "evaluate") return async (...args) => { await replace(); return target.evaluate(...args); };
        const value = Reflect.get(target, property);
        return typeof value === "function" ? value.bind(target) : value;
      } });
      assert.deepEqual(await assertControls(observedPage, "main", ["Pause after current round"]), [{ label: "Pause after current round", hit: true }]);
      assert.equal(replacements, 1);
      assert.equal(await page.evaluate(() => window.invocations), 0, "capture must not invoke or replay a mutation");
    } finally { await browser.close(); }
  });
}

test("renderer capture failures retain the original task failure metadata", async () => {
  const directory = await mkdtemp(join(tmpdir(), "rsi-task-failure-"));
  try {
    const page = { async screenshot() { throw new Error("renderer exited during screenshot"); },
      async content() { throw new Error("renderer exited during HTML capture"); } };
    await recordTaskFailure(page, directory, { error: "original task assertion", measurements: [{ label: "last completed phase" }] });
    const saved = JSON.parse(await readFile(join(directory, "failure.json"), "utf8"));
    assert.equal(saved.error, "original task assertion");
    assert.equal(saved.measurements[0].label, "last completed phase");
    assert.equal(saved.capture_errors.length, 2);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("engine selection cannot produce an empty task run", () => {
  assert.throws(() => browserNames("webkit"), /Unsupported RSI_WEB_BROWSER/);
  assert.deepEqual(browserNames(undefined), ["chromium", "firefox"]);
  assert.deepEqual(browserNames("firefox"), ["firefox"]);
});

test("task assertions reject missing, hidden, disabled and obscured controls and product notices", async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    page.setDefaultTimeout(1000);
    for (const markup of ["", '<button style="display:none">Complete result</button>',
      '<button disabled>Complete result</button>', '<button>Complete result</button><div style="position:fixed;inset:0"></div>']) {
      await page.setContent(`<main>${markup}</main>`);
      await assert.rejects(() => assertControls(page, "main", ["Complete result"]));
    }
    await page.setContent('<main><div style="height:1500px"></div><button>Complete result</button></main><div id="notice"></div><div class="pane-notice"></div>');
    assert.deepEqual(await assertControls(page, "main", ["Complete result"]), [{ label: "Complete result", hit: true }]);
    await assertNoNotices(page);
    await page.locator(".pane-notice").evaluate(node => {
      node.hidden = true;
      node.textContent = "Protected investigation. Use Deployment checks.";
    });
    await assertNoNotices(page);
    await page.locator(".pane-notice").evaluate(node => { node.hidden = false; });
    await assert.rejects(() => assertNoNotices(page), /Unexpected product notice/);
    await page.locator(".pane-notice").evaluate(node => { node.textContent = ""; });
    await page.locator("body").evaluate(node => node.insertAdjacentHTML("beforeend", '<div class="resource-content"></div>'));
    for (const feedback of ["Goal control rejected. Request old: command revision conflict", "Control outcome is unresolved. Request original."]) {
      await page.locator(".resource-content").evaluate((node, text) => { node.textContent = text; }, feedback);
      await assert.rejects(() => assertNoNotices(page), /Unexpected Goal feedback/);
    }
    await page.locator(".resource-content").evaluate(node => { node.textContent = ""; });
    for (const selector of ["#notice", ".pane-notice"]) {
      await page.locator(selector).evaluate(node => { node.textContent = "Failed to open result"; });
      await assert.rejects(() => assertNoNotices(page), /Unexpected product notice/);
      await page.locator(selector).evaluate(node => { node.textContent = ""; });
    }
  } finally { await browser.close(); }
});

for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
  test(`${name}: persistence evidence reads the current database version and waits for a saved record`, async () => {
    const { waitForSavedPresentation } = await import('./presentation.mjs');
    const browser = await engine.launch({ headless: true });
    try {
      const page = await browser.newPage();
      await page.route('http://layout.invalid/**', route => route.fulfill({body:'<main></main>', contentType:'text/html'}));
      await page.goto('http://layout.invalid/');
      await page.evaluate(async () => {
        const db = await new Promise((resolve,reject) => {
          const request=indexedDB.open('rsi.presentation',2);
          request.onupgradeneeded=()=>request.result.createObjectStore('layouts');
          request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
        });
        window.saveFixtureLayout=()=>new Promise((resolve,reject)=>{
          const transaction=db.transaction('layouts','readwrite');
          transaction.objectStore('layouts').put({layout:'saved'},'fixture');
          transaction.oncomplete=resolve;transaction.onerror=()=>reject(transaction.error);
        });
      });
      await assert.rejects(waitForSavedPresentation(page, 100), /saved layout|Timeout/);
      const pending = waitForSavedPresentation(page);
      await page.evaluate(()=>window.saveFixtureLayout());
      assert.equal(await pending, true);
    } finally { await browser.close(); }
  });
}
