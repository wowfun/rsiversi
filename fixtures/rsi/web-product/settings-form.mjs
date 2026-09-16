import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';
export async function verifySettingsForm(browser,root) {
  const context=await browser.newContext();
  await context.route('http://settings.fixture/**',async route=>route.fulfill({status:200,contentType:route.request().url().endsWith('.js') ? 'text/javascript' : 'text/html',body:route.request().url().endsWith('.js') ? await readFile(join(root,'plugins/rsi/web/settings-form.js'),'utf8') : '<!doctype html><form></form>'}));
  const page=await context.newPage();
  try {
    await page.goto('http://settings.fixture/');
    const result=await page.evaluate(async()=>{
      const {settingsForm,canUseSettingsForm}=await import('/settings-form.js');
      const schema={type:'object',properties:{name:{type:'string'},text:{type:'string'},enabled:{type:'boolean'},count:{type:'integer',minimum:1,maximum:10},mode:{enum:['one','two']},optional:{type:'string'},complex:{type:'array',items:{type:'string'}}},required:['name','text','enabled','count','mode','complex']};
      const value={name:'<script>alert(1)</script>',text:'first\r\nsecond',enabled:false,count:2,mode:'one',complex:['kept']};
      const form=settingsForm(schema,value);document.querySelector('form').append(form.element);
      const original=form.read();
      const crlfEditor=form.element.querySelector('[aria-label="Settings / text JSON"]');
      if (!crlfEditor) throw new Error("CRLF strings need JSON before editing");
      crlfEditor.value=JSON.stringify('first\r\nsecond changed');
      const editedCrlf=form.read().text;
      const find=name=>document.querySelector(`[aria-label="${name}"]`);
      find('Settings / name').value='changed';find('Settings / enabled').checked=true;find('Settings / count').value='7';find('Settings / mode').value='1';
      find('Include Settings / optional').checked=true;find('Include Settings / optional').dispatchEvent(new Event('change'));find('Settings / optional').value='added';
      const changed=form.read();
      find('Include Settings / optional').checked=false;const omitted=form.read();
      find('Settings / count').value='10000000000000000000';let exact=false;try{form.read()}catch(error){exact=String(error).includes('exact integer')}
      find('Settings / count').value='5';find('Settings / complex JSON').value='[';let invalid=false;try{form.read()}catch(error){invalid=String(error).includes('valid JSON')}
      const numeric=settingsForm({type:'number'}, 2);document.querySelector('form').append(numeric.element);
      const numericInput=numeric.element.querySelector('input');
      const refused=[];
      for (const text of ['10000000000000000001','1.0000000000000001','1e19']) { numericInput.value=text;try{numeric.read();refused.push(false)}catch{refused.push(true)} }
      numericInput.value='1.0000000000000001';const exactText=numeric.json();
      numericInput.value='0.125';const exactDecimal=numeric.read();
      const nested=settingsForm({type:'array'}, []);nested.element.querySelector('textarea').value='[18446744073709551615]';const nestedText=nested.json();let nestedExact=false;try{nested.read()}catch{nestedExact=true}
      const unknown=settingsForm({type:'object',properties:{known:{type:'string'}}},{unknown:'keep'});const retained=unknown.read();
      const untrusted=settingsForm({type:'string',format:'uri'},'https://example.test');
      return {editedCrlf,original,changed,omitted,exact,invalid,retained,refused,exactDecimal,nestedExact,exactText,nestedText,fallback:!!untrusted.element.querySelector('textarea'),unsafe:canUseSettingsForm('{"id":18446744073709551615}'),string:canUseSettingsForm('{"id":"18446744073709551615"}'),decimal:canUseSettingsForm('{"number":1.0000000000000001}'),negative:canUseSettingsForm('{"id":-9223372036854775808}'),escaped:canUseSettingsForm('{"text":"\\\" : 18446744073709551615"}'),script:document.scripts.length};
    });
    assert.equal(result.editedCrlf,"first\r\nsecond changed");assert.equal(result.exactText,"1.0000000000000001");assert.equal(result.nestedText,"[18446744073709551615]");assert(result.refused.every(Boolean));assert(result.nestedExact);assert.equal(result.exactDecimal,0.125);assert.equal(result.original.text,'first\r\nsecond');assert.equal(result.original.enabled,false);assert.equal(result.original.name,'<script>alert(1)</script>');assert.equal(result.changed.name,'changed');assert.equal(result.changed.enabled,true);assert.equal(result.changed.count,7);assert.equal(result.changed.mode,'two');assert.equal(result.changed.optional,'added');assert(!Object.hasOwn(result.omitted,'optional'));assert.deepEqual(result.retained,{unknown:'keep'});assert(result.exact&&result.invalid&&result.fallback);assert(!result.unsafe&&!result.negative&&!result.decimal&&result.string&&result.escaped);assert.equal(result.script,0);return result;
  } finally {await context.close()}
}
