import {useEffect,useRef,useState} from 'react';
import {source,useView,type Provider,type Setup} from '../bridge.ts';
import {Button} from '../button.tsx';
import {Modal} from '../presentation.tsx';
import type {SetupActions} from './setup-actions.ts';
import {ModelDefinitions} from './model-definitions.tsx';
import {draftRows,modelDefinitions,newRow,providerLabels,providerOwners,providerModelConfig,replaceProvider,type Limits} from './provider-draft.ts';
import css from './models.module.css';

function waitSetup(ready:(setup:Setup)=>boolean):Promise<Setup> {
  return new Promise((resolve,reject)=>{
    let stop=()=>{};
    const timer=setTimeout(()=>{stop();reject(new Error('Configuration saved; refresh settings to read its status.'));},10000);
    const read=()=>{const setup=source.getSnapshot()?.setup;if(setup&&ready(setup)){clearTimeout(timer);stop();resolve(setup);}};
    stop=source.subscribe(read);read();
  });
}
export function ProviderEditor({original,revision,deployments,actions,close}:{original?:Provider;revision:string;deployments:Provider[];actions:SetupActions;close:()=>void}) {
  const setup=useView(view=>view?.setup),secret=useRef<HTMLInputElement>(null),alive=useRef(true);
  const [provider,setProvider]=useState<Provider['provider']>(original?.provider??'deepseek');
  const uniqueId=(kind:string)=>{let id=kind,index=2;while(deployments.some(entry=>entry.config.deployment===id))id=`${kind}-${index++}`;return id;};
  const [deployment,setDeployment]=useState(String(original?.config.deployment??uniqueId('deepseek')));
  const [endpoint,setEndpoint]=useState(String(original?.config.endpoint??'https://api.deepseek.com'));
  const [slot,setSlot]=useState(String((original?.config.credential as {slot?:string}|undefined)?.slot??(deployments.some(entry=>entry.provider==='deepseek')?uniqueId('deepseek'):'default')));
  const [protocol,setProtocol]=useState(String(original?.config.protocol??'responses')),[path,setPath]=useState(String(original?.config.path??'/chat/completions'));
  const [images,setImages]=useState(original?.config.allow_image_input===true),[imageOutput,setImageOutput]=useState(original?.config.image===true),[language,setLanguage]=useState(original?.config.language!==false);
  const [rows,setRows]=useState(()=>draftRows(original)),[error,setError]=useState(''),[note,setNote]=useState(''),[removing,setRemoving]=useState(false);
  const [candidates,setCandidates]=useState<{id:string;name?:string|null;context_window_tokens?:number|null;max_output_tokens?:number|null}[]>();
  const [advanced,setAdvanced]=useState(false),[applied,setApplied]=useState(false);
  const [checking,setChecking]=useState(false);
  const statusRead=useRef<{timer?:ReturnType<typeof setTimeout>;pending?:Promise<unknown>;checked?:string}>({});
  const {busy,command,act}=actions,locked=busy||!setup?.allowed;
  const credential=setup?.credential?.provider===provider&&setup.credential.slot===slot?setup.credential.status:undefined;
  useEffect(()=>{alive.current=true;return()=>{alive.current=false;if(secret.current)secret.current.value='';};},[]);
  useEffect(()=>{
    const state=statusRead.current,key=JSON.stringify([provider,slot]);
    if(busy||checking||!setup?.credentials_available||!slot.trim()||state.checked===key)return;
    state.timer=setTimeout(()=>{
      state.timer=undefined;state.checked=key;setChecking(true);
      state.pending=command({kind:'credential_status',provider,slot}).catch(()=>{}).finally(()=>{
        state.pending=undefined;if(alive.current)setChecking(false);
      });
    },300);
    return()=>clearTimeout(state.timer);
  },[provider,slot,busy,checking,setup?.credentials_available]);
  const choose=(kind:Provider['provider'])=>{
    setProvider(kind);setDeployment(kind==='openai-compatible'?'':uniqueId(kind));setSlot(deployments.some(entry=>entry.provider===kind)?uniqueId(kind):'default');
    setEndpoint(kind==='deepseek'?'https://api.deepseek.com':kind==='openai'?'https://api.openai.com':'');setRows(kind==='deepseek'?[newRow('deepseek-flash')]:[]);setCandidates(undefined);setError('');setNote('');if(secret.current)secret.current.value='';
  };
  const perform=(work:()=>Promise<void>)=>{
    clearTimeout(statusRead.current.timer);
    act(async()=>{setError('');try{await statusRead.current.pending;await work();}catch(reason){if(alive.current)setError(reason instanceof Error?reason.message:String(reason));}});
  };
  const save=()=>perform(async()=>{
    const models=modelDefinitions(rows);
    if(!Object.keys(models).length&&(provider!=='openai'||language))throw new Error('Add at least one model ID.');
    if(!/^[a-zA-Z][a-zA-Z0-9._-]{0,127}$/.test(deployment))throw new Error('Provider ID must start with a letter and contain only letters, numbers, dots, underscores or hyphens.');
    const url=new URL(endpoint);if(!['http:','https:'].includes(url.protocol)||url.username||url.password||url.search||url.hash)throw new Error('Enter an HTTP or HTTPS API base URL without credentials, query or fragment.');
    const config:Record<string,unknown>={...providerModelConfig(provider,models,original?.config),deployment,endpoint,credential:{owner:providerOwners[provider],slot}};
    if(provider==='deepseek')config.protocol=protocol;
    if(provider==='openai'){config.language=language;config.image=imageOutput;}
    if(provider==='openai-compatible'){config.path=path;config.allow_image_input=images;}
    const next=replaceProvider(deployments,{provider,config},original?String(original.config.deployment):undefined);
    if(!applied){
      if(setup?.providers?.desired_revision!==revision)throw new Error('Provider configuration changed. Cancel and reopen this editor before applying.');
      const key=secret.current?.value??'';
      if(key){if(secret.current)secret.current.value='';await command({kind:'credential_set',provider,slot,secret:key});if(alive.current)setNote('API key saved.');}
      await command({kind:'providers_replace',expected_revision:revision,deployments:next});
      setApplied(true);
    }
    const current=await waitSetup(value=>value.providers?.desired_revision!==revision);
    if(current.providers?.diagnostic||current.providers?.applying||current.providers?.desired_revision!==current.providers?.applied_revision)throw new Error('Provider saved, but not applied. Refresh settings to inspect its status.');
    if(!current.agent?.default_model&&current.settings_writable&&rows[0])await command({kind:'default_model',ticket:current.ticket,model:{deployment,model:rows[0].name.trim()}});
    if(alive.current)close();
  });
  const fetchModels=()=>perform(async()=>{
    const previous=source.getSnapshot()?.setup?.discovery;
    await command({kind:'discover_models',input:{request:{provider,endpoint,slot},secret:secret.current?.value||null}});
    const next=await waitSetup(value=>!!value.discovery&&value.discovery!==previous&&value.discovery.request.provider===provider&&value.discovery.request.endpoint===endpoint&&value.discovery.request.slot===slot);
    if(alive.current)setCandidates(next.discovery!.models);
  });
  const addCandidate=(candidate:NonNullable<typeof candidates>[number])=>{
    const row=newRow(candidate.id),limits=row.limits as Limits;
    if(candidate.context_window_tokens)limits.context_window_tokens=candidate.context_window_tokens;
    limits.max_output_reserve_tokens=Math.min(candidate.max_output_tokens??limits.max_output_reserve_tokens,limits.context_window_tokens-1);
    limits.default_output_reserve_tokens=Math.min(limits.default_output_reserve_tokens,limits.max_output_reserve_tokens);
    setRows(previous=>previous.some(row=>row.name===candidate.id)?previous:[...previous,row]);
  };
  return <form className={css.editor} onSubmit={event=>{event.preventDefault();save();}} aria-label={original?'Edit model provider':'Add model provider'} aria-busy={busy}>
    <fieldset disabled={locked||applied}>
      {!original&&<><div className={css.segments} role="group" aria-label="Provider type"><button type="button" aria-pressed={provider!=='openai-compatible'} onClick={()=>choose('deepseek')}>Built-in provider</button><button type="button" aria-pressed={provider==='openai-compatible'} onClick={()=>choose('openai-compatible')}>Custom model API</button></div>
      {provider!=='openai-compatible'?<label>Provider<select aria-label="Provider" value={provider} onChange={event=>choose(event.target.value as Provider['provider'])}><option value="deepseek">DeepSeek</option><option value="openai">OpenAI</option></select></label>:<><label>Provider ID<input aria-label="Provider ID" value={deployment} onChange={event=>{setDeployment(event.target.value);setSlot(event.target.value||'default');}} maxLength={128} placeholder="my-gateway"/></label><label>Base URL<input aria-label="Base URL" value={endpoint} onChange={event=>setEndpoint(event.target.value)} placeholder="https://gateway.example/v1" maxLength={2048}/></label></> }</>}
      {original&&<div className={css.editorTitle}><strong>{providerLabels[provider]}</strong><span>{deployment}</span></div>}
      <label>API key<input ref={secret} aria-label="API key" type="password" autoComplete="off" spellCheck={false} disabled={!setup?.credentials_available||credential?.editable===false} placeholder={credential?.availability.kind==='configured'?'Leave blank to use saved credentials':'Enter your API key'} /></label>
      {credential?.editable===false&&<p className={css.hint}>This key is managed by the Service environment.</p>}
      <details open={advanced} onToggle={event=>setAdvanced(event.currentTarget.open)}><summary>Customized settings</summary><div className={css.advanced}>
        {(original||provider!=='openai-compatible')&&<label>Base URL<input aria-label="Base URL" value={endpoint} onChange={event=>setEndpoint(event.target.value)} maxLength={2048}/></label>}
        <label>API protocol<select aria-label="API protocol" value={provider==='deepseek'?protocol:provider==='openai'?'responses':'chat-completions'} onChange={event=>setProtocol(event.target.value)} disabled={provider!=='deepseek'}>{provider!=='openai-compatible'&&<option value="responses">OpenAI Responses</option>}{provider!=='openai'&&<option value="chat-completions">OpenAI Chat Completions</option>}</select></label>
        {provider==='openai-compatible'&&<><label>Request path<input aria-label="Request path" value={path} onChange={event=>setPath(event.target.value)}/></label><label className={css.check}><input type="checkbox" checked={images} onChange={event=>setImages(event.target.checked)}/>Image input</label></>}
        {provider==='openai'&&<div className={css.checks}><label className={css.check}><input type="checkbox" checked={language} onChange={event=>setLanguage(event.target.checked)}/>Language models</label><label className={css.check}><input type="checkbox" checked={imageOutput} onChange={event=>setImageOutput(event.target.checked)}/>Image generation</label></div>}
        <div className={css.modelsHeading}><strong>Models</strong><Button size="sm" variant="outline" disabled={!endpoint||!setup?.credentials_available} onClick={fetchModels}>Fetch available models</Button></div>
        {candidates&&<div className={css.candidates} aria-label="Available models">{!candidates.length?<p>No models returned.</p>:candidates.map(candidate=><button type="button" key={candidate.id} disabled={rows.some(row=>row.name===candidate.id)} onClick={()=>addCandidate(candidate)}>{candidate.name??candidate.id}<span>{rows.some(row=>row.name===candidate.id)?'Added':'Add'}</span></button>)}</div>}
        <ModelDefinitions rows={rows} setRows={setRows}/>
        <details className={css.connection}><summary>Connection identifiers</summary><label>Provider ID<input aria-label="Provider ID" value={deployment} disabled={!!original} onChange={event=>setDeployment(event.target.value)} maxLength={128}/></label><label>Credential slot<input aria-label="Credential slot" value={slot} onChange={event=>setSlot(event.target.value)} maxLength={128}/></label></details>
        {original&&<Button className={css.danger} size="sm" onClick={()=>setRemoving(true)}>Remove provider</Button>}
      </div></details>
    </fieldset>
    {note&&<p className={css.hint} role="status">{note}</p>}{error&&<p className={css.error} role="alert">{error}</p>}
    <footer><Button variant="outline" disabled={busy} onClick={close}>Cancel</Button><Button variant="primary" type="submit" disabled={locked}>{busy?'Applying…':applied?'Finish setup':original?'Apply':'Create provider'}</Button></footer>
    {removing&&<Modal label="Remove provider" className={css.confirm} close={()=>{if(!busy)setRemoving(false);}} initialFocus='[data-cancel]'><h2>Remove provider</h2><p>Remove “{deployment}” from the model list? Saved credentials and conversations will be kept.</p><footer><Button data-cancel disabled={busy} onClick={()=>setRemoving(false)}>Cancel</Button><Button variant="outline" className={css.danger} disabled={busy} onClick={()=>perform(async()=>{await command({kind:'providers_replace',expected_revision:revision,deployments:deployments.filter(entry=>entry.config.deployment!==deployment)});close();})}>Remove provider</Button></footer></Modal>}
  </form>;
}
