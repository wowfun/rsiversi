import {settingsEditor} from './settings-editor.js';
import {settingsFocus} from './settings-focus.js';
const node=(tag,text='',className='')=>{const result=document.createElement(tag);result.textContent=text;result.className=className;return result};
const version=editor=>JSON.stringify(editor.version);

// One retained editor per registered section, owned by the Settings window.
export function settingsDrafts(command) {
  const drafts=new Map();
  return {
    clear(){drafts.clear()},
    render(editor){
      const slot=editor.namespace==='rsi.client'?'appearance':'advanced';
      let draft=drafts.get(slot);
      if(!draft||draft.namespace!==editor.namespace){draft=settingsDraft(editor,command);drafts.set(slot,draft)}
      else draft.update(editor);
      return draft.element;
    },
  };
}
function settingsDraft(initial,command) {
  const form=node('form');let current=initial,base,fields,fieldset,save,failure,reload,dirty=false,saving=false,awaiting,message='';
  const changed=()=>version(current)!==base;
  const controls=()=>{
    const disabled=saving||!!awaiting||!current.description.writable;
    const restore=fieldset.disabled!==disabled?settingsFocus(form):()=>{};
    fieldset.disabled=disabled;
    save.disabled=saving||!!awaiting||changed()||!current.description.writable||!fields.canSave();
    reload.hidden=!changed()&&!awaiting;reload.disabled=saving;
    reload.textContent=awaiting?'Reload settings':'Discard edits and reload';
    failure.textContent=message||(awaiting?'Waiting for refreshed settings. Reload settings if this does not finish.':changed()?'Settings changed while this draft was open. Copy your edits or discard them and reload before saving.':'');restore();
  };
  const draw=editor=>{
    const restore=settingsFocus(form);message='';
    current=editor;base=version(editor);dirty=false;awaiting=undefined;
    form.dataset.settingsTicket=editor.ticket;form.dataset.settingsNamespace=editor.namespace;
    const description=editor.description,metadata=description.metadata;
    const applies={live:'Applies live',new_session:'Applies to new conversations',restart:'Restart required'}[metadata.applies];
    form.replaceChildren(node('p',applies,'settings-applies'),node('p',metadata.description,'hint'));
    for(const [label,value] of [['Schema',metadata.schema],['Defaults',description.defaults]]){
      const disclosure=node('details','','settings-description');disclosure.append(node('summary',label),node('pre',JSON.stringify(value,null,2)));form.append(disclosure);
    }
    if(metadata.sensitive_fields.length)form.append(node('p',`Sensitive fields: ${metadata.sensitive_fields.map(path=>path.join(' / ')||'(root)').join(', ')}`,'hint'));
    fields=settingsEditor(metadata.schema,editor.text,!description.writable);
    fieldset=node('fieldset');fieldset.append(fields.element);
    failure=node('p','','settings-error settings-field-error');failure.setAttribute('role','alert');
    save=node('button','Save settings','primary');save.type='submit';
    reload=node('button','Discard edits and reload');reload.type='button';
    reload.addEventListener('click',async()=>{
      const ticket=current.ticket;saving=true;message='';controls();
      try {await command({action:'settings_read',namespace:current.namespace});dirty=false;awaiting=ticket;if(current.ticket!==ticket)draw(current)}
      catch(error){message=String(error.message??error);controls()}
      finally{saving=false;controls()}
    });
    fields.element.addEventListener('input',()=>{dirty=true;controls()});
    form.append(node('p','Saving requires the version you opened.','hint'),fieldset,failure,save,reload);
    if(!description.writable)form.append(node('p','Settings provider is read-only.','hint'));
    if(editor.namespace!=='rsi.client'){
      const browse=node('button','Browse registered settings');browse.type='button';
      browse.addEventListener('click',()=>command({action:'settings_list'}).catch(error=>{message=String(error.message??error);controls()}));form.append(browse);
    }
    controls();restore();
  };
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(save.disabled)return;
    const ticket=current.ticket;saving=true;message='';controls();
    try {
      await command({action:'settings_save',ticket,text:fields.json()});
      dirty=false;awaiting=ticket;
      if(current.ticket!==ticket)draw(current);
    }catch(error){message=String(error.message??error);controls()}
    finally{saving=false;controls()}
  });
  draw(initial);
  return {namespace:initial.namespace,element:form,update(editor){
    current=editor;
    form.dataset.settingsTicket=editor.ticket;
    if(!dirty&&!saving&&(!awaiting||editor.ticket!==awaiting))draw(editor);
    else controls();
  }};
}
