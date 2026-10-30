import type {ResourceModel} from './details/model.ts';
import {observable} from './bridge.ts'

export interface PanelEntry {
  view:string; open_revision:string; opening_request:string|null; session:string|null; pane:string|null; generation:string|null; floating:boolean
  reopen:Record<string,unknown>|null
}
export interface ResourceHost {
  body:HTMLElement; key:string|undefined; title:string; entry:PanelEntry
  model?:ResourceModel; rendererRoot?:HTMLElement|null
  rendererKey:string; surface:string
  show(key:string,title:string,content:ResourceModel):void
  clear():void
}
// The renderer owns these DOM roots. Dock tabs only position them; they never
// recreate a presentation or derive action authority from a layout identity.
export const resourceHosts = new Map<string,ResourceHost>()
export const resources = observable<readonly ResourceHost[]>([])
let published:readonly {host:ResourceHost;title:string;entry:PanelEntry;key:ResourceHost['key'];model:ResourceHost['model']}[]=[]
export function publishResources() {
  const next=[...resourceHosts.values()].map(host=>({host,title:host.title,entry:host.entry,key:host.key,model:host.model}))
  if(next.length===published.length&&next.every((item,index)=>item.host===published[index].host&&item.title===published[index].title&&item.entry===published[index].entry&&item.key===published[index].key&&item.model===published[index].model))return
  published=next;resources.set(next.map(item=>item.host))
}
export function clearResources() {
  for (const host of resourceHosts.values()) host.body.remove()
  resourceHosts.clear(); publishResources()
}
