import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';

export async function verifySettingsRuntime(browser) {
  const fixture = await sourceFixture({name:'settings-runtime-fixture',source:`
import {createSettingsRuntime} from './src/runtime/settings-runtime.js';
import {settingsPanel,settingsMounted,registeredSettings} from './src/bridge.ts';
const calls=[],context={connection:{},view:{settings_revision:'1'},boundCall:()=>async(method,payload)=>calls.push(JSON.parse(payload)),notify(message){throw Error(message)}};
const runtime=createSettingsRuntime(context);
window.fixture={calls,runtime,registeredSettings,present(value){context.view=value;runtime.present(value)}};
settingsMounted.set(true);
`});
  const page = await browser.newPage(), errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(fixture.url);
    await page.waitForFunction(() => !!window.fixture);
    const result = await page.evaluate(async () => {
      const {runtime,registeredSettings,present,calls} = window.fixture;
      await runtime.command({action:'settings_list'});
      present({settings_revision:'2',settings_catalog:{ticket:'2',page:{namespaces:['rsi.agent']}}});
      const namespaces = registeredSettings.getSnapshot()?.catalog.page.namespaces;
      await runtime.command({action:'settings_read',namespace:'rsi.agent',window:'2'});
      present({settings_revision:'2',settings_catalog:{ticket:'3',namespace:'rsi.agent'}});
      const loadingNamespace = registeredSettings.getSnapshot()?.catalog.namespace;
      const editor = {namespace:'rsi.agent',ticket:'editor',text:'{}',version:{revision:1},description:{writable:true,defaults:{},metadata:{schema:{},applies:'new_session',sensitive_fields:[]}}};
      present({settings_revision:'2',settings:editor});
      const selectedTicket = registeredSettings.getSnapshot()?.draft.current.ticket;
      await runtime.command({action:'settings_read',namespace:'rsi.agent'});
      present({settings_revision:'2',settings:editor});
      const oldWindowFenced = registeredSettings.getSnapshot() === undefined;
      present({settings_revision:'4',settings:{...editor,ticket:'fresh'}});
      const freshTicket = registeredSettings.getSnapshot()?.draft.current.ticket;
      runtime.dismiss();
      present({settings_revision:'4',settings:editor});
      const dismissedWindowFenced = registeredSettings.getSnapshot()?.draft.current.ticket === 'fresh';
      runtime.dispose();
      return {namespaces,loadingNamespace,selectedTicket,oldWindowFenced,freshTicket,dismissedWindowFenced,actions:calls.map(value=>value.action)};
    });
    assert.deepEqual(result,{namespaces:['rsi.agent'],loadingNamespace:'rsi.agent',selectedTicket:'editor',oldWindowFenced:true,freshTicket:'fresh',dismissedWindowFenced:true,actions:['settings_list','settings_read','settings_read']});
    assert.deepEqual(errors,[]);
    return result;
  } finally {
    await page.close();
    await fixture.close();
  }
}
