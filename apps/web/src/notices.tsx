import {useSyncExternalStore} from 'react'
import {notices} from './bridge.ts'

export function Notices() {
  const {transient, storage} = useSyncExternalStore(notices.subscribe, notices.getSnapshot)
  return <div id="notice" className="notice" role="alert" hidden={!transient && !storage}>
    {transient && <p>{transient}</p>}{storage && <p>{storage}</p>}
  </div>
}
