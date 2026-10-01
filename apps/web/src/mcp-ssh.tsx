import {useState} from 'react'
import {Button} from './button.tsx'
import type {SshView} from './ssh-targets.tsx'
interface Target {host_epoch:string;target:string;server:string}
interface Config {id:string;enabled:boolean;tools:string[];resource_templates:boolean;transport:{kind:'ssh_stdio';target:string;command:string;arguments:string[];cwd:string;environment:Record<string,unknown>}}
export interface McpSshView {available:boolean;state:{target:Target;revision:string;config:Config|null;apply_error:unknown}|null;uncertain:boolean;notice:string|null}
export function McpSsh({view,ssh,busy,command}:{view:McpSshView;ssh?:SshView;busy:boolean;command:(value:Record<string,unknown>)=>void}) {
  const [target,setTarget]=useState(''),[server,setServer]=useState('')
  const selection={host_epoch:ssh?.catalog?.host_epoch??'',target,server}
  const current=view.state?.target.target===target&&view.state.target.server===server&&view.state.target.host_epoch===selection.host_epoch?view.state:undefined
  return <section className="mcp-panel mcp-ssh-panel" aria-label="SSH MCP servers"><h2>MCP over SSH</h2>
    <p className="hint">Select a target and exact server name. A Local operator grants administration for this pair and its credential references. Connecting also requires Use.</p>
    <label>Execution target<select aria-label="MCP execution target" value={target} disabled={busy} onChange={event=>setTarget(event.target.value)}><option value="">Choose an SSH target</option>{ssh?.catalog?.targets.map(item=><option key={item.candidate.target} value={item.candidate.target}>{item.candidate.name}</option>)}</select></label>
    <label>Server name<input aria-label="SSH MCP server name" maxLength={128} value={server} disabled={busy} onChange={event=>setServer(event.target.value)}/></label>
    <Button disabled={busy||!target||!server||!selection.host_epoch} onClick={()=>command({kind:'read',target:selection})}>Read server configuration</Button>
    {!ssh?.catalog&&<p className="hint">Refresh SSH targets to select an execution target.</p>}
    {view.notice&&<p role="status">{view.notice}</p>}
    {view.uncertain&&<p role="alert">Read the current configuration before another change.</p>}
    {current&&<Editor key={`${target}:${server}:${current.revision}`} state={current} locked={busy||view.uncertain} command={command}/>}
  </section>
}
function Editor({state,locked,command}:{state:NonNullable<McpSshView['state']>;locked:boolean;command:(value:Record<string,unknown>)=>void}) {
  const saved=state.config
  const [program,setProgram]=useState(saved?.transport.command??''),[cwd,setCwd]=useState(saved?.transport.cwd??''),[args,setArgs]=useState(JSON.stringify(saved?.transport.arguments??[],null,2)),[env,setEnv]=useState(JSON.stringify(saved?.transport.environment??{},null,2)),[tools,setTools]=useState(saved?.tools.join('\n')??''),[enabled,setEnabled]=useState(saved?.enabled??false),[templates,setTemplates]=useState(saved?.resource_templates??false),[error,setError]=useState('')
  const change={target:state.target,expected:state.revision,config:null}
  const save=()=>{try{
    const argumentsValue=JSON.parse(args),environment=JSON.parse(env)
    if(!Array.isArray(argumentsValue)||argumentsValue.some(item=>typeof item!=='string'))throw new Error('Arguments must be a JSON array of strings')
    if(!environment||typeof environment!=='object'||Array.isArray(environment))throw new Error('Environment must be a JSON object')
    setError('');command({kind:'put',change:{...change,config:{id:state.target.server,enabled,tools:tools.split('\n').map(value=>value.trim()).filter(Boolean),resource_templates:templates,transport:{kind:'ssh_stdio',target:state.target.target,command:program,arguments:argumentsValue,cwd,environment}}}})
  }catch(error){setError(String(error))}}
  return <form onSubmit={event=>{event.preventDefault();save()}}><fieldset disabled={locked}>
    <p className="hint">Configuration revision {state.revision}. Saving does not grant credentials or start a Session.</p>
    <label>Target executable<input aria-label="MCP target executable" value={program} required onChange={event=>setProgram(event.target.value)}/></label>
    <label>Target working directory<input aria-label="MCP target working directory" value={cwd} required onChange={event=>setCwd(event.target.value)}/></label>
    <label>Arguments (JSON array)<textarea aria-label="MCP arguments" value={args} onChange={event=>setArgs(event.target.value)} maxLength={65536}/></label>
    <label>Environment (JSON)<textarea aria-label="MCP environment" value={env} onChange={event=>setEnv(event.target.value)} maxLength={65536}/></label>
    <p className="hint">Use credential references such as {JSON.stringify({TOKEN:{kind:'credential',reference:{owner:'rsi.mcp',slot:'server-token'}}})}. Secret values are resolved by the Service. Target HOME and PATH are selected on the target.</p>
    <label>Selected tools (one per line)<textarea aria-label="MCP selected tools" value={tools} onChange={event=>setTools(event.target.value)} maxLength={32768}/></label>
    <label className="check"><input type="checkbox" checked={enabled} onChange={event=>setEnabled(event.target.checked)}/>Enable this server</label>
    <label className="check"><input type="checkbox" checked={templates} onChange={event=>setTemplates(event.target.checked)}/>Allow this server’s dynamic resource templates</label>
    {error&&<p role="alert">{error}</p>}
    <div className="actions"><Button type="submit">Save SSH MCP server</Button><Button type="button" disabled={!saved} onClick={()=>command({kind:'refresh',change})}>Connect SSH MCP server</Button><Button type="button" disabled={!saved} onClick={()=>command({kind:'remove',change})}>Remove SSH MCP server</Button></div>
  </fieldset></form>
}
