import { SlotCore, type SlotRendererHost, type SlotScopeAdapter, type StandardSourceBinding, type ScopedStandardSourceBinding } from '@rsi/dsh-slots'
import { createSlotRenderer } from '../vendor/dsh/renderer/scoped-slots.tsx'
import { observable, source, selection } from './bridge.ts'
import {failPresentation} from './frame-commit.tsx'

export const slots = new SlotCore()
slots.onEntryError((_key,_entry,error)=>failPresentation(error))
const empty: StandardSourceBinding = {key: undefined, hooks: {}, keyedHooks: {}, props: {}}
const current = observable<StandardSourceBinding>(empty)
const absent = observable<StandardSourceBinding>(empty)
let binding: ScopedStandardSourceBinding | undefined
function updateScope() {
  const surface = source.getSnapshot()?.surfaces[selection.getSnapshot()]
  const key = surface ? `${selection.getSnapshot()}:${surface.generation}:${surface.session}` : undefined
  if (key === current.getSnapshot().key) return
  binding = key ? {...empty, key, ctx: Object.freeze({key}), props: {surface: selection.getSnapshot()}} : undefined
  current.set(binding ?? empty)
}
source.subscribe(updateScope); selection.subscribe(updateScope)
const sessionAdapter: SlotScopeAdapter = {current, bindingSource: () => absent,
  renderArea(binding, props) { return binding.key === undefined ? props.empty?.() ?? null : props.children },
}
export const host: SlotRendererHost = {
  subscribe: slots.subscribe.bind(slots), getVersion: slots.getVersion.bind(slots), entriesOf: slots.entries.bind(slots),
  entriesOfSlot: slots.entriesOfSlot.bind(slots), reportEntryError: slots.reportEntryError.bind(slots), specOf: slots.specDynamic.bind(slots), isLive: slots.isLive.bind(slots),
  reportFactoryError: slots.reportFactoryError.bind(slots),
  subscribeFactory: slots.subscribeFactory.bind(slots),
  getFactoryVersion: slots.factoryVersion.bind(slots),
  factoryOf: slots.factory.bind(slots), isFactoryLive: slots.isFactoryLive.bind(slots),
  storeOf(entry) { if (entry.store) throw new Error('RSIversi slot composition does not install a DSH store engine'); return undefined },
  factoryStoreOf(definition) { if (definition.store) throw new Error('RSIversi factories do not install a DSH store engine'); return undefined },
  retainFactoryOccurrence(definition) { if (definition.store) throw new Error('RSIversi factories do not install a DSH store engine'); return () => {} },
  root: observable(empty), scopeRevision: observable(0),
  // RSI has no explicit SessionProvider targets; its selected surface is inherited.
  scope() { return sessionAdapter },
}
export const renderer = createSlotRenderer()
