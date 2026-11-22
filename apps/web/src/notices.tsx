import {useSyncExternalStore} from 'react'
import {notices, setCommandWarning} from './bridge.ts'
import {Toast} from '../vendor/dsh/dockkit-primitives/Toast.tsx'
import {IconWarningOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx'

export function Notices() {
  const {transient, storage, warning} = useSyncExternalStore(notices.subscribe, notices.getSnapshot)
  return <><div id="notice" className="notice" role="alert" hidden={!transient && !storage}>
    {transient && <p>{transient}</p>}{storage && <p>{storage}</p>}
  </div>{warning && <Toast key={warning.id} text={warning.text} anchor={warning.anchor}
    icon={<IconWarningOutlineRegular/>} onDone={()=>setCommandWarning()}/>}</>
}
