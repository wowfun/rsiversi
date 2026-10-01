// Rendering-only DTOs; no Service, grant, SSH or credential evidence.
import React,{useLayoutEffect,useRef} from 'react'
import {createRoot} from 'react-dom/client'
import {Setup} from '@fixture/web/src/setup.tsx'
import {source,installActions} from '@fixture/web/src/bridge.ts'
import '@fixture/web/styles.css'
import '@fixture/web/src/workbench.css'
import '@fixture/web/vendor/dsh/dockkit-tokens.css'
const theme=new URLSearchParams(location.search).get('theme')??'light'
document.documentElement.dataset.theme=theme
if(theme==='dark')document.body.setAttribute('data-ds-dark-theme','')
source.set({catalog:{models:[],models_more:false},application_surfaces:[],surfaces:{},
 setup:{allowed:true,receipts:[],providers:{deployments:[],desired_revision:'1',applied_revision:'1',applying:false}},
 plugins:{target:{kind:'host'},guidance:[],page:null,ssh:{available:true,can_trust:false,uncertain:false,notice:null,directory:null,
  catalog:{host_epoch:'fixture',targets:[{candidate:{target:'a'.repeat(32),name:'Build server',endpoint:{host:'build.example.test',port:22,user:'developer'}},revision:'1',fingerprint:'SHA256:'+'A'.repeat(43),connection_epoch:'1',connected:true,unavailable_programs:[],permissions:{use_target:false,manage:false}}]}},
  mcp_ssh:{available:true,state:null,uncertain:false,notice:null}}
} as any)
installActions({command:async()=>{},open:async()=>{},select:()=>{},closeSurface:async()=>{},addSurface:async()=>{},call:async()=>{}})
function Scene(){const dialog=useRef<HTMLDialogElement>(null);useLayoutEffect(()=>{dialog.current!.showModal()},[]);return <dialog ref={dialog} className="settings-modal" aria-label="Settings"><Setup close={()=>{}}/></dialog>}
createRoot(document.getElementById('root')!).render(<Scene/> )
