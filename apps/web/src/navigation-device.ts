import {useEffect,useRef,useState} from 'react'
import {bindSnapshotSelector} from '../vendor/dsh/renderer/bind.ts'
import {NavigationCommandLane} from '../navigation-command-lane.js'
import {DeviceStore,defaultPreferences} from '../device-store.js'
import {workspaceMembers} from '../workspace-tree.js'
import {presentationIdentity} from './presentation.tsx'
import {input,source,type Navigation,type ManualOrder,type ExecutionLocation} from './bridge.ts'

export interface DeviceNavigation {view:'workspace'|'workspace_tree'|'flat';sessionOrder:'updated'|'manual';workspaceOrder:'updated'|'manual'}
type Position='before'|'after'|'first'|'last'|'previous'|'next'
const useIdentity=bindSnapshotSelector(presentationIdentity)
const scope='sessions:all'
const navigationLane=new NavigationCommandLane()
function members(order:ManualOrder) {
  if(order.seed.membership.kind!=='available')throw new Error('Manual ordering is paused: complete membership exceeds 1024 sessions or 128 KiB.')
  return [...order.seed.membership.members].sort((a,b)=>{
    const left=BigInt(a.last_activity_ms),right=BigInt(b.last_activity_ms)
    return left===right?(a.session<b.session?1:-1):left>right?-1:1
  }).map(member=>({id:member.session,partition:`${member.group}:${member.pinned}:${member.archived}`}))
}
function waitSeed(previous?:string):Promise<ManualOrder> {
  return new Promise((resolve,reject)=>{
    let unsubscribe=()=>{}
    const timer=setTimeout(()=>{unsubscribe();reject(new Error('Complete membership did not arrive; refresh navigation.'))},10000)
    const read=()=>{
      const order=source.getSnapshot()?.navigation?.order
      if(order && order.ticket!==previous && order.seed.scope.kind==='all') {clearTimeout(timer);unsubscribe();resolve(order)}
    }
    unsubscribe=source.subscribe(read);read()
  })
}
export function useDeviceNavigation(nav:Navigation|undefined,workspaces:{id:string;path:string;location:ExecutionLocation}[],moreWorkspaces:boolean) {
  const identity=useIdentity(value=>value)
  const [preferences,setPreferences]=useState<DeviceNavigation>(defaultPreferences as DeviceNavigation)
  const [workspaceIds,setWorkspaceIds]=useState<string[]>([])
  const [ordered,setOrdered]=useState<string[]>([]),[notice,setNotice]=useState(''),[ready,setReady]=useState(false),[offset,setOffset]=useState(0)
  const lifetime=useRef(0),owner=useRef<DeviceStore>(),intents=useRef(new NavigationCommandLane()),seedRequest=useRef(''),lastReconciled=useRef(''),workspaceReconciled=useRef(''),summaryRequest=useRef('')
  useEffect(()=>{
    lifetime.current++
    let live=true,store:DeviceStore|undefined,unsubscribers:(()=>void)[]=[]
    setReady(false);setPreferences(defaultPreferences as DeviceNavigation);setOrdered([]);setWorkspaceIds([]);setOffset(0);setNotice('');seedRequest.current='';lastReconciled.current='';workspaceReconciled.current=''
    const read=async()=>{
      if(!store)return
      try{
        const [prefs,order,workspaceOrder]=await Promise.all([store.read('preferences'),store.read('orders',scope),store.read('orders','workspaces')])
        if(live){setPreferences(prefs.value);setOrdered(order.value.ids);setWorkspaceIds(workspaceOrder.value.ids);setReady(true);setNotice('')}
      }catch(error){if(live)setNotice(String(error))}
    }
    const focus=()=>{void read()}
    if(identity)void DeviceStore.open(identity).then(async opened=>{
      const connected=opened as DeviceStore
      store=connected
      if(!live){connected.close();return}
      owner.current=connected;await read()
      if(!live)return
      unsubscribers=[connected.subscribe('preferences','',focus),connected.subscribe('orders',scope,focus),connected.subscribe('orders','workspaces',focus)]
      window.addEventListener('focus',focus)
    },()=>{if(live)setNotice('Device navigation preferences are unavailable. Updated ordering remains available.')})
    return()=>{lifetime.current++;live=false;unsubscribers.forEach(stop=>stop());window.removeEventListener('focus',focus);store?.close();if(owner.current===store)owner.current=undefined}
  },[identity])
  const serial=(work:()=>Promise<void>)=>{
    const generation=lifetime.current
    return intents.current.run(async()=>{
      if(generation!==lifetime.current)throw new Error('Connection changed before saving navigation preferences.')
      await work();setNotice('')
    }).catch(error=>{setNotice(String(error));throw error})
  }
  const command=(value:Record<string,unknown>,current?:()=>boolean)=>{
    const store=owner.current,generation=lifetime.current
    return navigationLane.run(()=>{
      if(store!==owner.current || generation!==lifetime.current)throw new Error('Connection changed before navigation dispatch.')
      if(current && !current())return
      return input.command(value)
    })
  }
  const seed=async()=>{
    const latest=source.getSnapshot()?.navigation
    const previous=latest?.order?.ticket
    await command({action:'navigate',command:{kind:'order_seed',scope:{kind:'all'}}})
    return waitSeed(previous)
  }
  const commit=async(intent:Record<string,unknown>,mode:'manual'|'updated')=>{
    const store=owner.current;if(!store)throw new Error('Device navigation preferences are unavailable.')
    const [order,prefs]=await store.applyAll([
      {bucket:'orders',scope,intent},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{sessionOrder:mode}}},
    ])
    if(owner.current!==store)return
    setOrdered(order.value.ids);setPreferences(prefs.value)
  }
  const move=(id:string,position:Position,relative:string|null=null)=>serial(async()=>{
    const store=owner.current
    const complete=await seed()
    if(store!==owner.current)throw new Error('Connection changed before saving order.')
    await commit({kind:'move',members:members(complete),id,position,relative},'manual')
    setOffset(0)
  })
  const mode=(value:'manual'|'updated')=>serial(async()=>{
    if(value==='updated')await commit({kind:'updated'},value)
    else {const store=owner.current;const complete=await seed();if(store!==owner.current)throw new Error('Connection changed before saving order.');await commit({kind:'reconcile',members:members(complete)},value)}
    setOffset(0)
  })
  const view=async(value:DeviceNavigation['view'])=>serial(async()=>{
    const store=owner.current;if(!store)throw new Error('Device preferences are unavailable.')
    const next=await store.apply('preferences',{kind:'patch',patch:{view:value}})
    if(owner.current===store)setPreferences(next.value)
  })
  const workspaceKey=JSON.stringify(workspaceMembers(workspaces))
  useEffect(()=>{
    if(!ready || moreWorkspaces || preferences.workspaceOrder!=='manual' || workspaceReconciled.current===workspaceKey)return
    const store=owner.current;if(!store)return
    workspaceReconciled.current=workspaceKey
    void store.reconcileNavigation('workspaces',JSON.parse(workspaceKey)).then(
      ([order,prefs])=>{if(owner.current===store){setWorkspaceIds(order.value.ids);setPreferences(prefs.value)}},
      error=>{if(owner.current===store)setNotice(String(error))},
    )
  },[ready,moreWorkspaces,preferences.workspaceOrder,workspaceKey])
  const workspaceMove=(id:string,position:Position,relative:string|null=null)=>serial(async()=>{
    if(moreWorkspaces)throw new Error('Complete workspace membership is unavailable; the saved order is preserved.')
    const store=owner.current;if(!store)throw new Error('Device preferences are unavailable.')
    const [order,prefs]=await store.applyAll([
      {bucket:'orders',scope:'workspaces',intent:{kind:'move',members:workspaceMembers(workspaces),id,position,relative}},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{workspaceOrder:'manual'}}},
    ])
    if(owner.current===store){setWorkspaceIds(order.value.ids);setPreferences(prefs.value)}
  })
  const workspaceUpdated=()=>serial(async()=>{
    const store=owner.current;if(!store)throw new Error('Device preferences are unavailable.')
    const [order,prefs]=await store.applyAll([
      {bucket:'orders',scope:'workspaces',intent:{kind:'updated'}},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{workspaceOrder:'updated'}}},
    ])
    if(owner.current===store){setWorkspaceIds(order.value.ids);setPreferences(prefs.value)}
  })
  useEffect(()=>{
    if(!ready || preferences.sessionOrder!=='manual' || !nav?.ticket)return
    if(nav.order?.seed.scope.kind==='all' && nav.order.seed.metadata_revision===nav.metadata_revision)return
    const key=`${identity}:${nav.metadata_revision}`
    if(seedRequest.current===key)return
    seedRequest.current=key
    void seed().catch(error=>setNotice(String(error)))
  },[ready,preferences.sessionOrder,nav?.order,nav?.metadata_revision,nav?.ticket,identity])
  const ticket=nav?.order?.ticket
  useEffect(()=>{
    if(!ready || preferences.sessionOrder!=='manual' || !nav?.order || nav.order.seed.membership.kind!=='available' || lastReconciled.current===ticket)return
    lastReconciled.current=ticket??''
    const store=owner.current;if(!store)return
    void store.reconcileNavigation(scope,members(nav.order)).then(
      ([order,prefs])=>{if(owner.current===store){setOrdered(order.value.ids);setPreferences(prefs.value)}},
      error=>{if(owner.current===store)setNotice(String(error))},
    )
  },[ready,preferences.sessionOrder,ticket])
  const complete=nav?.order?.seed.membership
  const manual=preferences.sessionOrder==='manual' && complete?.kind==='available' && !nav?.filter.query
  const selected=manual?new Map(complete.members.filter(member=>!member.pinned && member.archived===nav?.filter.archived).map(member=>[member.session,member])):new Map()
  const all=ordered.filter(id=>selected.has(id)),requested=all.slice(offset,offset+64),requestKey=JSON.stringify(requested)
  useEffect(()=>{setOffset(value=>Math.min(value,Math.max(0,Math.floor((all.length-1)/64)*64)))},[all.length])
  useEffect(()=>{
    if(!manual || !ticket || !lastReconciled.current || JSON.stringify(nav?.order?.requested)===requestKey)return
    const request=`${ticket}:${requestKey}`
    if(summaryRequest.current===request)return
    summaryRequest.current=request
    void command({action:'navigate',command:{kind:'order_summaries',ticket,sessions:JSON.parse(requestKey)}},()=>summaryRequest.current===request && source.getSnapshot()?.navigation?.order?.ticket===ticket).catch(error=>setNotice(String(error))).finally(()=>{if(summaryRequest.current===request)summaryRequest.current=''})
  },[manual,ticket,requestKey,nav?.order?.requested])
  const loaded=JSON.stringify(nav?.order?.requested)===requestKey
  const entries=manual && loaded?(nav?.order?.entries??[]).flatMap(row=>row?[row]:[]):[]
  return {preferences,notice,ready,command,view,mode,move,workspaceIds,workspaceMove,workspaceUpdated,manual,loading:manual&&!loaded,entries,ordered,offset,total:all.length,
    paused:preferences.sessionOrder==='manual' && complete?.kind==='too_large',
    previous:()=>setOffset(value=>Math.max(0,value-64)),next:()=>setOffset(value=>Math.min(Math.max(0,Math.floor((all.length-1)/64)*64),value+64)),
    refresh:()=>{seedRequest.current='';lastReconciled.current='';void command({action:'navigate',command:{kind:'order_seed',scope:{kind:'all'}}}).catch(error=>setNotice(String(error)))},
  }
}
