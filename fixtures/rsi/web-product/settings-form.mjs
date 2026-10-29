import assert from 'node:assert/strict';
import {sourceFixture} from './source-fixture.mjs';
export async function verifySettingsForm(browser,root) {
 const schema={type:'object',properties:{name:{type:'string'},text:{type:'string'},enabled:{type:'boolean'},count:{type:'integer',minimum:1,maximum:10},mode:{enum:['one','two']},optional:{type:'string'},complex:{type:'array',items:{type:'string'}}},required:['name','text','enabled','count','mode','complex']};
 const value={name:'<script>alert(1)</script>',text:'first\r\nsecond',enabled:false,count:2,mode:'one',complex:['kept']};
 const server=await sourceFixture({name:'settings-form-fixture',source:`
import React from 'react';import {createRoot} from 'react-dom/client';import {flushSync} from 'react-dom';
import {RegisteredDraft} from './src/settings/registered.tsx';import {createSettingsDraft} from './src/runtime/settings-draft.js';import {formUnavailable} from './settings-json.js';
const root=createRoot(document.getElementById('root'));window.fixture={formUnavailable,reset(schema,value){this.draft?.retire();this.draft=createSettingsDraft({namespace:'fixture',ticket:'one',version:'1',text:JSON.stringify(value),description:{writable:true,defaults:{},metadata:{schema,applies:'live',description:'',sensitive_fields:[]}}},async()=>{});flushSync(()=>root.render(<RegisteredDraft draft={this.draft}/>))}};fixture.reset(${JSON.stringify(schema)},${JSON.stringify(value)});
`});
 const page=await browser.newPage(),errors=[];page.on('pageerror',error=>errors.push(error.message));
 try {
  await page.goto(server.url);await page.getByLabel('Settings / name',{exact:true}).waitFor();
  const original=await page.evaluate(()=>JSON.parse(fixture.draft.fields.json()));assert.deepEqual(original,value);
  await page.getByLabel('Settings / text JSON',{exact:true}).fill(JSON.stringify('first\r\nsecond changed'));
  await page.getByLabel('Settings / name',{exact:true}).fill('changed');await page.getByLabel('Settings / enabled',{exact:true}).check();await page.getByLabel('Settings / count',{exact:true}).fill('7');await page.getByLabel('Settings / mode',{exact:true}).selectOption('1');
  await page.getByLabel('Include Settings / optional',{exact:true}).check();await page.getByLabel('Settings / optional',{exact:true}).fill('added');
  const changed=await page.evaluate(()=>JSON.parse(fixture.draft.fields.json()));assert.deepEqual(changed,{...value,name:'changed',text:'first\r\nsecond changed',enabled:true,count:7,mode:'two',optional:'added'});
  await page.getByLabel('Include Settings / optional',{exact:true}).uncheck();assert.equal(await page.evaluate(()=>Object.hasOwn(JSON.parse(fixture.draft.fields.json()),'optional')),false);
  await page.getByLabel('Settings / complex JSON',{exact:true}).fill('[');assert.equal(await page.getByRole('button',{name:'Save settings',exact:true}).isDisabled(),true);
  await page.evaluate(()=>fixture.reset({type:'number'},2));await page.getByLabel('Settings',{exact:true}).fill('1.0000000000000001');assert.equal(await page.evaluate(()=>fixture.draft.fields.json()),'1.0000000000000001');
  await page.getByRole('button',{name:'Edit as JSON',exact:true}).click();assert.equal(await page.getByLabel('Settings JSON',{exact:true}).inputValue(),'1.0000000000000001');
  await page.getByRole('button',{name:'Use form',exact:true}).click();await page.getByRole('status').filter({hasText:'exact numbers'}).waitFor();
  await page.evaluate(()=>fixture.reset({type:'array'},[]));await page.getByLabel('Settings JSON',{exact:true}).filter({visible:true}).fill('[18446744073709551615]');assert.equal(await page.evaluate(()=>fixture.draft.fields.json()),'[18446744073709551615]');
  await page.evaluate(()=>fixture.reset({type:'object',properties:{known:{type:'string'}}},{unknown:'keep'}));assert.deepEqual(await page.evaluate(()=>JSON.parse(fixture.draft.fields.json())),{unknown:'keep'});
  await page.evaluate(()=>fixture.reset({type:'string',format:'uri'},'https://example.test'));assert.equal(await page.locator('textarea[aria-label="Settings JSON"]:visible').count(),1);
  const admission=await page.evaluate(()=>['{"id":18446744073709551615}','{"id":"18446744073709551615"}','{"number":1.0000000000000001}','{"id":-9223372036854775808}'].map(text=>fixture.formUnavailable(text)===''));assert.deepEqual(admission,[false,true,false,false]);assert.deepEqual(errors,[]);
  return {original,changed,exactNumbers:true,crlf:true,optionalFields:true,invalidJson:true,unknownProperties:true,unsupportedSchema:true,errors};
 } finally {await page.close();await server.close()}
}
