import {settingsForm,canUseSettingsForm} from './settings-form.js';
export const MAXIMUM_SETTINGS_BYTES=1024*1024;
const limitMessage="This value exceeds the Web editor's 1 MiB input limit.";
const encoder=new TextEncoder();
const oversized=text=>text.length>MAXIMUM_SETTINGS_BYTES||(text.length>MAXIMUM_SETTINGS_BYTES/3&&encoder.encode(text).length>MAXIMUM_SETTINGS_BYTES);
export function formUnavailable(text) {
  if(oversized(text))return limitMessage;
  try { JSON.parse(text); } catch { return 'Enter valid JSON before switching to the form.'; }
  return canUseSettingsForm(text)?'':'This value needs JSON to preserve its exact numbers.';
}
export function settingsEditor(schema, original, disabled=false) {
  const initialOversized=oversized(original),locked=disabled||initialOversized;
  const element=document.createElement('div'),host=document.createElement('div'),text=document.createElement('textarea'),toggle=document.createElement('button'),reason=document.createElement('p');
  element.className='settings-editor';
  text.className='settings-text';text.value=original;text.spellcheck=false;text.readOnly=locked;text.setAttribute('aria-label','Settings JSON');
  toggle.type='button';reason.className='hint';reason.setAttribute('role','status');
  let jsonMode=!!formUnavailable(original),fields,formEdited=false;
  const build=()=>{formEdited=false;fields=settingsForm(schema,JSON.parse(text.value),locked);host.replaceChildren(fields.element)};
  if(!jsonMode)build();
  host.addEventListener('input',()=>{formEdited=true});
  const value=()=>jsonMode||!formEdited?text.value:fields.json();
  let checkedText,checkedError,checkedOversized;
  const check=()=>{const text=value();if(text!==checkedText){checkedText=text;checkedOversized=oversized(text);checkedError=undefined;try{if(checkedOversized)throw new Error(limitMessage);JSON.parse(text)}catch(error){checkedError=error}}return text};
  const json=()=>{const text=check();if(checkedError)throw checkedError;return text};
  const canSave=()=>{if(locked)return false;try{json();return true}catch{return false}};
  const update=()=>{check();const why=jsonMode&&checkedOversized?limitMessage:'';toggle.textContent=jsonMode?'Use form':'Edit as JSON';toggle.disabled=initialOversized||!!why;reason.textContent=why;reason.hidden=!why;host.hidden=jsonMode;text.hidden=!jsonMode};
  toggle.addEventListener('click',()=>{
    if(jsonMode){const why=formUnavailable(text.value);if(why){reason.hidden=false;reason.textContent=why;return}build();jsonMode=false;update();host.querySelector('input,select,textarea')?.focus()}
    else {try{text.value=value();jsonMode=true;update();text.focus()}catch(error){reason.hidden=false;reason.textContent=String(error.message??error)}}
  });
  text.addEventListener('input',update);update();element.append(host,toggle,text,reason);
  return {element,json,canSave};
}
