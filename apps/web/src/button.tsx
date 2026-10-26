import type {ComponentProps} from 'react'
import {Button as VendorButton} from '../vendor/dsh/primitives/Button.tsx'
import {closeDisclosure} from '../disclosure.js'
import {report} from './bridge.ts'

// Product geometry sits outside the immutable vendor closure.
export function Button({className,size='md',onClick,...props}:ComponentProps<typeof VendorButton>) {
  return <VendorButton {...props} onClick={onClick?event=>{const target=event.currentTarget;closeDisclosure(target);try{void Promise.resolve(onClick(event)).catch(report)}catch(error){report(error)}}:undefined} size={size} data-rsi-size={size} className={`rsi-button ${className??''}`}/>
}
