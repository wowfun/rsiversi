import type {ComponentProps} from 'react'
import {Button as VendorButton} from '../vendor/dsh/primitives/Button.tsx'

// Product geometry sits outside the immutable vendor closure.
export function Button({className,size='md',...props}:ComponentProps<typeof VendorButton>) {
  return <VendorButton {...props} size={size} data-rsi-size={size} className={`rsi-button ${className??''}`}/>
}
