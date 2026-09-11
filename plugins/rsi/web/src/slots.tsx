import { SlotCore, type SlotRendererHost, type StandardSourceBinding, type ScopedStandardSourceBinding } from '@rsi/dsh-slots'
import { createSlotRenderer } from '../vendor/dsh/renderer/scoped-slots.tsx'
import { observable, source, selection } from './bridge.ts'

export const slots = new SlotCore()
slots.onEntryError((key,_entry,error)=>console.error(`Slot ${key} failed`,error))
const empty: StandardSourceBinding = {key: undefined, hooks: {}, keyedHooks: {}, props: {}}
const current = observable<StandardSourceBinding>(empty)
let binding: ScopedStandardSourceBinding | undefined
function updateScope() {
  const surface = source.getSnapshot()?.surfaces[selection.getSnapshot()]
  const key = surface ? `${selection.getSnapshot()}:${surface.generation}:${surface.session}` : undefined
  if (key === current.getSnapshot().key) return
  binding = key ? {...empty, key, ctx: Object.freeze({key}), props: {surface: selection.getSnapshot()}} : undefined
  current.set(binding ?? empty)
}
source.subscribe(updateScope); selection.subscribe(updateScope)
export const host: SlotRendererHost = {
  subscribe: slots.subscribe.bind(slots), getVersion: slots.getVersion.bind(slots), entriesOf: slots.entries.bind(slots),
  entriesOfSlot: slots.entriesOfSlot.bind(slots), reportEntryError: slots.reportEntryError.bind(slots), specOf: slots.specDynamic.bind(slots), isLive: slots.isLive.bind(slots),
  storeOf(entry) { if (entry.store) throw new Error('RSI slot composition does not install a DSH store engine'); return undefined },
  root: observable(empty), scopeRevision: observable(0),
  scope() { return {current, resolve: key => binding?.key === key ? binding : undefined} },
}
export const renderer = createSlotRenderer()
