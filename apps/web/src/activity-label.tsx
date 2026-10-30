import type {AttentionEntry} from './bridge.ts'

export function ActivityLabel({state, iconOnly = false}: {state: AttentionEntry['status']|undefined; iconOnly?: boolean}) {
  if(state===undefined)return null
  const label = {running:'Running',waiting:'Waiting for you',unread:'New activity',unknown:'Unknown'}[state]
  return <span className="session-activity" title={label} role={iconOnly?'img':undefined} aria-label={iconOnly?label:undefined}><span className={`session-state ${state}`} aria-hidden="true"/>
    {!iconOnly && label}
  </span>
}
