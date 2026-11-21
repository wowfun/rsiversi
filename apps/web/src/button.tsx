import {forwardRef,type ComponentPropsWithoutRef} from 'react'
import {Button as VendorButton} from '../vendor/dsh/primitives/Button.tsx'
import {closeDisclosure} from '../disclosure.js'
import {report} from './bridge.ts'

// Product geometry sits outside the immutable vendor closure.
export const Button=forwardRef<HTMLButtonElement,ComponentPropsWithoutRef<typeof VendorButton>>(function Button({className,size='md',onClick,...props},ref) {
  return <VendorButton {...props} ref={ref} onClick={onClick?event=>{const target=event.currentTarget;closeDisclosure(target);try{void Promise.resolve(onClick(event)).catch(report)}catch(error){report(error)}}:undefined} size={size} data-rsi-size={size} className={`rsi-button ${className??''}`}/>
})
