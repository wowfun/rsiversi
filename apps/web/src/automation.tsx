import {useEffect,useState} from 'react'
import {Button} from './button.tsx'
import {input,useView} from './bridge.ts'

type Row={id:string;rule:string;environment:string;url:string;sha:string;state:string;exploration:string;verdict:string|null;created_ms:string}
type Attempt={id:string;state:string;current_rule_revision:string|null;may_cancel:boolean;may_resume:boolean;deployment:{url:string;sha:string};rule:{id:string};result:{outcome:string;final_url:string;assertions:{assertion:Record<string,string>;passed:boolean;detail:string}[];snapshot:string;evidence_error:string|null}|null;exploration:string;report:string|null;session_id:string|null}
export interface AutomationView{status?:{readiness:string};page?:{entries:Row[];after:string;watermark:string;more:boolean};attempt?:Attempt;mutation?:{id:string;state:string}}
const command=(request:Record<string,unknown>)=>input.command({action:'automation',request})
function EvidenceImage({bytes,attempt}:{bytes:Uint8Array<ArrayBuffer>;attempt:string}){
  const [source,setSource]=useState('')
  useEffect(()=>{
    const url=URL.createObjectURL(new Blob([bytes],{type:'image/png'}))
    setSource(url)
    return ()=>URL.revokeObjectURL(url)
  },[bytes])
  return source?<img className="automation-screenshot" src={source} alt={`Deployment evidence for attempt ${attempt}`}/>:null
}

export function Automation({close}:{close:()=>void}){
  const view=useView(value=>value?.automation),[busy,setBusy]=useState(false),[error,setError]=useState(''),[evidence,setEvidence]=useState<{id:string;bytes:Uint8Array<ArrayBuffer>}|null>(null)
  const work=async(action:()=>Promise<unknown>)=>{if(busy)return;setBusy(true);setError('');try{await action()}catch(error){setError(error instanceof Error?error.message:String(error))}finally{setBusy(false)}}
  const refresh=()=>work(async()=>{await command({operation:'status'});await command({operation:'list',after:'0',watermark:null,limit:50})})
  useEffect(()=>{void refresh()},[])
  const readAttempt=(id:string)=>{setEvidence(null);return command({operation:'get',id})}
  const attempt=view?.attempt
  const page=view?.page,receipt=view?.mutation
  const mutation=async(operation:'cancel'|'resume')=>{if(!attempt)return;await command({operation,id:attempt.id,request_id:crypto.randomUUID(),...(operation==='resume'?{rule_revision:attempt.current_rule_revision}:{})});await command({operation:'list',after:'0',watermark:null,limit:50});await readAttempt(attempt.id)}
  return <section className="automation-panel" aria-label="Deployment checks">
    <div className="automation-heading"><div><span className="eyebrow">Standing preview rules</span><h2>Deployment checks</h2><p>Fixed assertions determine acceptance. Exploration reports explain observed failures.</p></div><div className="automation-controls"><Button disabled={busy} onClick={()=>void refresh()}>Refresh</Button><Button onClick={close}>Close</Button></div></div>
    <p role="status">Browser: {view?.status?.readiness?.replaceAll('_',' ')??'Checking availability'}</p>
    {error&&<p role="alert" className="notice">{error}</p>}
    {receipt&&<p role="status">Attempt {receipt.id}: {receipt.state}. <Button disabled={busy} onClick={()=>void work(()=>readAttempt(receipt.id))}>Read attempt</Button></p>}
    <div className="automation-columns"><div className="automation-list">
      {!page?.entries.length&&<p>No visible deployment attempts.</p>}
      {page?.entries.map(row=><button key={row.id} className={`automation-row${attempt?.id===row.id?' selected':''}`} disabled={busy} onClick={()=>void work(()=>readAttempt(row.id))}><span><strong>{row.environment}</strong><small>Attempt {row.id} · {row.sha.slice(0,10)}</small></span><span className={`automation-verdict ${row.verdict??row.state}`}>{(row.verdict??row.state).replaceAll('_',' ')}</span></button>)}
      {page?.more&&<Button disabled={busy} onClick={()=>void work(()=>command({operation:'list',after:page.after,watermark:page.watermark,limit:50}))}>Next page</Button>}
    </div><div className="automation-detail">{attempt?<>
      <h3>Attempt {attempt.id}</h3><p className="automation-url">{attempt.deployment.url}</p>
      <div className="automation-controls"><Button disabled={busy||!attempt.may_cancel} onClick={()=>void work(()=>mutation('cancel'))}>Cancel</Button><Button disabled={busy||!attempt.may_resume||!attempt.current_rule_revision} onClick={()=>void work(()=>mutation('resume'))}>New attempt</Button>{attempt.session_id&&<Button disabled={busy} onClick={()=>void work(()=>input.open({action:'open',session:attempt.session_id}))}>Open investigation</Button>}</div>
      <h4>Deterministic check · {attempt.result?.outcome.replaceAll('_',' ')??attempt.state}</h4>
      {attempt.result?.assertions.map((row,index)=><div className="automation-assertion" key={index}><span aria-label={row.passed?'Passed':'Failed'}>{row.passed?'✓':'×'}</span><div><strong>{row.assertion.text??row.assertion.name??row.assertion.url}</strong><small>{row.detail}</small></div></div>)}
      {attempt.result&&<>{attempt.result.outcome!=='policy_blocked'&&<Button disabled={busy} onClick={()=>void work(async()=>{const reply=JSON.parse(await input.automationArtifact({operation:'artifact',id:attempt.id,ordinal:0}));const bytes=Uint8Array.from(atob(reply.png),character=>character.charCodeAt(0));setEvidence({id:attempt.id,bytes})})}>Read screenshot</Button>}{attempt.result.evidence_error&&<p>{attempt.result.evidence_error}</p>}<details><summary>Observed page text</summary><pre>{attempt.result.snapshot}</pre></details></>}
      {evidence?.id===attempt.id&&<EvidenceImage key={attempt.id} bytes={evidence.bytes} attempt={attempt.id}/>}
      <h4>Exploration · {attempt.exploration.replaceAll('_',' ')}</h4>{attempt.report&&<pre className="automation-report">{attempt.report}</pre>}
    </>:<p>Select an attempt to read its check evidence.</p>}</div></div>
  </section>
}
