import type {KeyedSnapshotSelectorHook} from '../vendor/dsh/slots/index.ts';

// Compile the public member form alongside the existing selector forms.
export function checkKeyedHook(useValue:KeyedSnapshotSelectorHook<number>) {
  const value:number|undefined=useValue('session','usage');
  const selected:string=useValue('session',value=>String(value));
  const compared:number=useValue('session',value=>value??0,Object.is);
  return {value,selected,compared};
}
