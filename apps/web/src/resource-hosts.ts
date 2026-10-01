import {observable} from './bridge.ts'

export interface PanelEntry {
  view:string; session:string|null; pane:string|null; generation:string|null; floating:boolean
  reopen:Record<string,unknown>|null
}
export interface ResourceHost {
  body:HTMLElement; key:string|undefined; title:string; entry:PanelEntry
  rendererKey:string; surface:string
  show(key:string,title:string,content:HTMLElement):void
  clear():void
}
// The renderer owns these DOM roots. Dock tabs only position them; they never
// recreate a presentation or derive action authority from a layout identity.
export const resourceHosts = new Map<string,ResourceHost>()
export const resources = observable<readonly ResourceHost[]>([])
let published:readonly {host:ResourceHost;metadata:string}[]=[]
export function publishResources() {
  const next=[...resourceHosts.values()].map(host=>({host,metadata:JSON.stringify([host.title,host.entry])}))
  if(next.length===published.length&&next.every((item,index)=>item.host===published[index].host&&item.metadata===published[index].metadata))return
  published=next;resources.set(next.map(item=>item.host))
}
export function clearResources() {
  for (const host of resourceHosts.values()) host.body.remove()
  resourceHosts.clear(); publishResources()
}
