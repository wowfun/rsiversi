import { useRef, useState } from 'react'
import { Button } from '../vendor/dsh/primitives/Button.tsx'
import { StateDot } from '../vendor/dsh/primitives/StateDot.tsx'
import { input, run, useView, type Provider } from './bridge.ts'
const owners = {deepseek:'rsi.ai.provider.deepseek',openai:'rsi.ai.provider.openai','openai-compatible':'rsi.ai.provider.openai-compatible'}
type Limits = {context_window_tokens: number; default_output_reserve_tokens: number; max_output_reserve_tokens: number}
type Row = {name: string; limits: Limits}
const newRow = (): Row => ({name:'',limits:{context_window_tokens:128000,default_output_reserve_tokens:4096,max_output_reserve_tokens:16384}})
export function Setup({close}: {close: () => void}) {
  const setup = useView(view => view?.setup), catalog = useView(view => view?.catalog), contributions = useView(view => view?.application_surfaces)
  const [provider,setProvider] = useState<Provider['provider']>('deepseek'), [slot,setSlot] = useState('default'), secret = useRef<HTMLInputElement>(null)
  const [deployment,setDeployment] = useState(''), [endpoint,setEndpoint] = useState('https://api.deepseek.com'), [protocol,setProtocol] = useState('responses')
  const [path,setPath] = useState('/chat/completions'), [images,setImages] = useState(false), [imageOutput,setImageOutput] = useState(false), [language,setLanguage] = useState(true)
  const [rows,setRows] = useState<Row[]>([newRow()]), [editing,setEditing] = useState<number>(), [preset,setPreset] = useState(''), [busy,setBusy] = useState(false)
  const command = (command: Record<string,unknown>) => input.command({action:'setup',command})
  const act = (work: () => Promise<unknown>) => void run(async () => {setBusy(true); try {await work()} finally {setBusy(false)}})
  const providers = setup?.providers
  const credential = setup?.credential?.provider === provider && setup.credential.slot === slot ? setup.credential.status : undefined
  const locked = !setup?.allowed || busy
  const load = (entry: Provider,index: number) => {
    setEditing(index); setProvider(entry.provider); setDeployment(String(entry.config.deployment)); setEndpoint(String(entry.config.endpoint));
    setSlot(String((entry.config.credential as {slot:string}).slot)); setProtocol(String(entry.config.protocol ?? 'responses'));
    setPath(String(entry.config.path ?? '/chat/completions')); setImages(entry.config.allow_image_input === true); setLanguage(entry.config.language !== false); setImageOutput(entry.config.image === true);
    setRows(Object.entries(entry.config.language_models as Record<string,Limits>).map(([name,limits]) => ({name,limits})))
  }
  const save = async () => {
    if (!providers) throw new Error('Refresh provider state before applying')
    if (new Set(rows.map(row=>row.name)).size !== rows.length) throw new Error('Model names must be unique')
    const config: Record<string,unknown> = {deployment,endpoint,credential:{owner:owners[provider],slot},language_models:Object.fromEntries(rows.map(row=>[row.name,row.limits]))}
    if (provider === 'deepseek') config.protocol=protocol
    if (provider === 'openai') {config.language=language; config.image=imageOutput}
    if (provider === 'openai-compatible') {config.path=path; config.allow_image_input=images}
    const next = [...providers.deployments], entry = {provider,config}
    if (editing === undefined) next.push(entry); else next[editing]=entry
    await command({kind:'providers_replace',expected_revision:providers.desired_revision,deployments:next}); setEditing(undefined)
  }
  return <section className="setup-panel" aria-label="Settings">
    <div className="setup-heading"><div><span className="eyebrow">Application settings</span><h1>Models & access</h1><p className="hint">Changes apply to new conversations. Each step saves independently.</p></div><Button onClick={close} aria-label="Close settings">×</Button></div>
    {!setup?.allowed && <p className="permission-note">Configuration is read only for this connection. A service administrator can grant this device access.</p>}
    <Button disabled={busy} size="sm" onClick={() => act(() => command({kind:'refresh'}))}>Refresh setup status</Button>
    {setup?.diagnostic && <p role="alert" className="settings-error">{setup.diagnostic}</p>}
    <div className="setup-grid">
    <section className="setup-section"><h2><span className="step">1</span>Credential</h2><p className="hint">Secret values are sent once and never shown in saved setup state.</p>
      <label>Provider<select aria-label="Provider" value={provider} onChange={e => {setProvider(e.target.value as Provider['provider']); setEditing(undefined); if(secret.current) secret.current.value=''}}><option value="deepseek">DeepSeek</option><option value="openai">OpenAI</option><option value="openai-compatible">OpenAI compatible</option></select></label>
      <label>Credential slot<input aria-label="Credential slot" value={slot} onChange={e => setSlot(e.target.value)} maxLength={128}/></label>
      <div className="actions"><Button size="sm" disabled={busy} onClick={() => act(() => command({kind:'credential_status',provider,slot}))}>Check credential</Button><span className="credential-status" role="status">{credential ? `${credential.availability.kind}${credential.editable ? '' : ' · read only'}` : 'Not checked'}</span></div>
      <form onSubmit={e => {e.preventDefault(); const value=secret.current?.value ?? ''; if(secret.current) secret.current.value=''; act(() => command({kind:'credential_set',provider,slot,secret:value}))}}><label>API key<input ref={secret} aria-label="API key" type="password" autoComplete="off" spellCheck={false} required disabled={locked || credential?.editable === false}/></label><div className="actions"><Button type="submit" variant="outline" disabled={locked || credential?.editable === false}>Save credential</Button><Button disabled={locked || credential?.editable === false} onClick={() => act(() => command({kind:'credential_unset',provider,slot}))}>Remove credential</Button></div></form>
    </section>
    <section className="setup-section provider-editor"><h2><span className="step">2</span>{editing === undefined ? 'Add provider' : 'Edit provider'}</h2><p className="hint">Endpoint and model limits follow your provider deployment. Enter the exact advertised model identifier.</p>
      <form onSubmit={e => {e.preventDefault(); act(save)}}><fieldset disabled={locked}>
        <label>Deployment name<input aria-label="Deployment name" required value={deployment} onChange={e=>setDeployment(e.target.value)} maxLength={128} placeholder="my-deepseek"/></label>
        <label>Endpoint<input aria-label="Provider endpoint" type="url" required value={endpoint} onChange={e=>setEndpoint(e.target.value)}/></label>
        {provider === 'deepseek' && <label>Protocol<select aria-label="DeepSeek protocol" value={protocol} onChange={e=>setProtocol(e.target.value)}><option value="responses">Responses</option><option value="chat-completions">Chat completions</option></select></label>}
        {provider === 'openai-compatible' && <><label>Request path<input value={path} onChange={e=>setPath(e.target.value)}/></label><label className="check"><input type="checkbox" checked={images} onChange={e=>setImages(e.target.checked)}/>Accept image input</label></>}
        {provider === 'openai' && <><label className="check"><input type="checkbox" checked={language} onChange={e=>setLanguage(e.target.checked)}/>Language models</label><label className="check"><input type="checkbox" checked={imageOutput} onChange={e=>setImageOutput(e.target.checked)}/>Image generation</label></>}
        {rows.map((row,index) => <div key={index} className="model-definition"><label>Model identifier<input aria-label={`Model identifier ${index+1}`} required value={row.name} onChange={e=>setRows(rows.map((item,i)=>i===index ? {...item,name:e.target.value} : item))}/></label><details><summary>Context and output limits</summary>{Object.entries(row.limits).map(([key,value]) => <label key={key}>{key.replaceAll('_',' ')}<input type="number" aria-label={`${key} ${index+1}`} min="1" required value={value} onChange={e=>setRows(rows.map((item,i)=>i===index ? {...item,limits:{...item.limits,[key]:Number(e.target.value)}} : item))}/></label>)}</details><Button size="sm" onClick={()=>setRows(rows.filter((_,i)=>i!==index))}>Remove model</Button></div>)}
        <Button size="sm" onClick={()=>setRows([...rows,newRow()])}>Add model</Button><div className="actions"><Button variant="primary" type="submit">Apply provider</Button>{editing !== undefined && <Button onClick={()=>setEditing(undefined)}>Cancel edit</Button>}</div>
      </fieldset></form>
    </section>
    <section className="setup-section"><h2>Managed deployments</h2><p className="hint">Desired {providers?.desired_revision ?? '—'} · Applied {providers?.applied_revision ?? '—'}</p>{providers?.diagnostic && <p role="alert" className="settings-error">{providers.diagnostic}</p>}
      {providers?.deployments.map((entry,index)=><div className="deployment-row" key={String(entry.config.deployment)}><StateDot state={providers.applying ? 'ongoing' : providers.desired_revision === providers.applied_revision ? 'done' : 'warning'}/><div><strong>{String(entry.config.deployment)}</strong><p className="hint">{entry.provider} · {String(entry.config.endpoint)}</p></div><Button size="sm" disabled={locked} onClick={()=>load(entry,index)}>Edit</Button><Button size="sm" disabled={locked} onClick={()=>act(()=>command({kind:'providers_replace',expected_revision:providers.desired_revision,deployments:providers.deployments.filter((_,i)=>i!==index)}))}>Remove</Button></div>)}
      {!providers?.deployments.length && <p className="hint">No managed deployments yet.</p>}
    </section>
    <section className="setup-section"><h2><span className="step">3</span>Conversation defaults</h2><p className="hint">Models supplied by the service Profile also appear here. Their deployments remain managed by that Profile.</p>
      <label>Default model<select aria-label="Default model" disabled={locked} value={setup?.agent?.default_model ? JSON.stringify(setup.agent.default_model) : ''} onChange={e=>{if(e.target.value) act(()=>command({kind:'default_model',ticket:setup?.ticket,model:JSON.parse(e.target.value)}))}}><option value="">Choose a model</option>{catalog?.models.map(model=><option key={JSON.stringify(model)} value={JSON.stringify(model)}>{model.model} · {model.deployment}</option>)}</select></label>
      {catalog?.models_more && <Button id="models-next" size="sm" onClick={()=>act(()=>input.command({action:'models_next'}))}>More models</Button>}
      <form onSubmit={e=>{e.preventDefault();act(()=>command({kind:'default_preset',ticket:setup?.ticket,preset}))}}><label>Default preset<input aria-label="Default preset" required placeholder={setup?.presets?.default ?? 'Existing preset identity'} value={preset} onChange={e=>setPreset(e.target.value)} disabled={locked}/></label><Button type="submit" size="sm" disabled={locked}>Save preset selection</Button></form>
    </section></div>
    <section className="setup-receipts" aria-label="Setup receipts"><h2>Operation results</h2>{setup?.receipts.map((receipt,index)=><p key={index} data-outcome={receipt.outcome}><strong>{receipt.operation} · {receipt.outcome}</strong> — {receipt.message}</p>)}</section>
    <section className="application-extensions"><h2>Application extensions</h2>{contributions?.map(item=><Button key={JSON.stringify(item.reference)} size="sm" onClick={()=>act(()=>input.command({action:"application_ui_surface",reference:item.reference}))}>{item.title}</Button>)}</section>
    <details className="advanced-settings"><summary>Additional settings</summary><Button size="sm" onClick={()=>act(()=>input.command({action:'settings_list'}))}>Open registered settings</Button></details>
  </section>
}
