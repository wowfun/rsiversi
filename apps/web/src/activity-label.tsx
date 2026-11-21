import type {AttentionEntry} from './bridge.ts'

export function ActivityLabel({state, failed, iconOnly = false}: {state: AttentionEntry['status']|undefined; failed?:boolean; iconOnly?: boolean}) {
  if(state===undefined)return null
  const label = {running:'Running',waiting:'Waiting for you',unread:failed?'Run failed':'Unread',unknown:'Unknown'}[state]
  return <span className="session-activity" title={label} role={iconOnly?'img':undefined} aria-label={iconOnly?label:undefined}><span className={`session-state ${state === "unread" && failed ? "failed" : state}`} aria-hidden="true"/>
    {!iconOnly && label}
  </span>
}
