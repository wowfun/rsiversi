import type { SshView } from "./ssh-targets.tsx"
import { bindSnapshotSelector } from '../vendor/dsh/renderer/bind.ts'
import type { LeafView } from './profile-leaves.tsx'
import type {AutomationView} from './automation.tsx'
import type {ResourceOwner} from './resource-owner.mjs'
import {updateNotice} from '../notice-state.js'

export interface Model { deployment: string; model: string }
export type ExecutionLocation = {kind:'local'} | {kind:'ssh';target:string}
export interface Coordinates {location:ExecutionLocation;path:string}
export interface OrderMember {session:string;group:number;pinned:boolean;archived:boolean;last_activity_ms:string}
export interface ManualOrder {ticket:string;seed:{scope:{kind:'all'}|{kind:'coordinates';coordinates:Coordinates};host_epoch:string;metadata_revision:string;membership:{kind:'too_large'}|{kind:'available';groups:Coordinates[];members:OrderMember[]}};requested:string[];entries:(NavigationEntry|null)[]}
export interface NavigationEntry { session: string; created_at_ms: string; last_activity_ms:string;location:ExecutionLocation; path: string; workspace: string | null; metadata: {title: string | null; archived: boolean; pinned: boolean} }
export interface AttentionPosition {conversation:{kind:'native'|'external';id:string};epoch:string;sequence:string}
export interface AttentionEntry {position:AttentionPosition;status:'waiting'|'running'|'unknown'|'unread';targets:({kind:'native';request:{kind:'approval'|'question';turn:string;request:string}}|{kind:'external';generation:string;request:string})[]}
export type WorkspaceFilter = {kind:'all'} | {kind:'registered';id:string} | {kind:'unregistered'}
export interface NavigationGroup {ticket:string;workspace:WorkspaceFilter;entries:NavigationEntry[];more:boolean;scanned:number;continued:boolean;stale:boolean}
export type PinnedEntry = {status:'available';entry:NavigationEntry} | {status:'missing';session:string;metadata:NavigationEntry['metadata']}
export interface Navigation { order?:ManualOrder|null;newer_activity:boolean;metadata_revision:string; ticket: string; filter: {query: string; archived: boolean|null; workspace: WorkspaceFilter}; pins:PinnedEntry[]; groups:Record<string,NavigationGroup>; entries: NavigationEntry[]; more: boolean; scanned: number; diagnostic: string | null; attention?:{entries:AttentionEntry[];truncated:boolean};attention_notice?:string }
export interface Provider { provider: 'deepseek' | 'openai' | 'openai-compatible'; config: Record<string, unknown> }
export interface Setup { discovery?:{request:{provider:string;endpoint:string;slot:string};models:{id:string;name?:string|null;context_window_tokens?:number|null;max_output_tokens?:number|null}[]}|null; ticket: string; allowed: boolean; credentials_available: boolean; providers_writable: boolean; settings_writable: boolean; agent: {default_model?: Model; reasoning_effort?: string | null} | null; default_model_description?: {model: Model; profile: {reasoning_efforts: {supported: string[]; default?: string | null}}} | null; presets: {default?: string} | null; providers: {desired_revision: string; applied_revision: string; applying: boolean; diagnostic: string | null; deployments: Provider[]} | null; credential?: {provider: string; slot: string; status: {editable: boolean; availability: {kind: 'configured'|'missing'|'unavailable'; source?: unknown}}}; diagnostic: string | null; receipts: {operation: string; outcome: string; message: string}[] }
export interface ExternalSnapshot {id:string;endpoint:string;cwd:string;status:string;completion:string|null;generation:string;epoch:string}
export interface ExternalView {conversation:{kind:'external';id:string};following:boolean;more:boolean;busy:boolean;observed:{snapshot:ExternalSnapshot;connected:boolean;permissions:unknown[]};blocks:unknown[];capabilities:{goal:boolean;preset:boolean;submit:boolean;load:boolean;resume:boolean};diagnostic:string|null}
export interface Surface { kind?:'native'|'external';protected?:boolean;external?:ExternalView;header:string;agent_preset:string;generation: string; session: string; path: string; workspace:string; transcript: {status: string; blocks: unknown[]}; ui_surfaces: {bundle:string;title: string; reference: {name:string;[key:string]:unknown}}[] }
export interface McpCredentialTarget {server:string;reference:{owner:string;slot:string}}
export interface McpServer {id:string;enabled:boolean;transport:'http'|'stdio'|'ssh_stdio';credential:{owner:string;slot:string}|null;epoch:string;last_verified_sha256:string|null;ready:boolean;error:string|null;tools:{name:string;selected:boolean}[]}
export interface McpStatus {settings_pending:boolean;fresh_ready:boolean;fresh_error:string|null;servers:McpServer[]}
export interface PluginsView {actions:string[];mcp_ssh?:import('./mcp-ssh.tsx').McpSshView;leaves?:LeafView;ssh?:SshView;target:{kind:string};guidance:string[];exa_available:boolean;exa_credential:{availability:{kind:string};editable:boolean}|null;exa_notice:string|null;mcp_available:boolean;mcp:McpStatus|null;mcp_notice:string|null;mcp_credential:{target:McpCredentialTarget;availability:{kind:string};editable:boolean}|null;ticket:string;diagnostic:string|null;page:{context:{target:{kind:string};availability:string;source_digest:string|null;preset_source:string|null};desired_revision:string;observed_revision:string;health:string|null;watcher:string|null;offset:number;total:number;next_offset:number|null;plugins:{instance:string;desired_plugin:string|null;enabled:boolean;origin:string;diagnostics:string[];observed:{plugin:string;state:string}|null}[]}|null}
export interface WorkspaceRecord {id:string;coordinates:Coordinates;name?:string|null;created_at_ms?:number|null}
export interface View { automation?:AutomationView|null; appearance?:{theme:"system"|"light"|"dark";content_font_size:number};preference_error?:string; external_catalog?:{endpoints:{id:string;enabled:boolean}[];conversations:ExternalSnapshot[];more:boolean};plugins?: PluginsView; application_surfaces: {bundle:string;title:string;reference:{name:string;[key:string]:unknown}}[]; surfaces: Record<string, Surface | null>; catalog: {workspaces: WorkspaceRecord[]; workspaces_more: boolean; workspace_order_seed:{kind:'available';records:WorkspaceRecord[]}|{kind:'too_large'}|null; models: Model[]; models_more: boolean}; navigation?: Navigation; setup?: Setup; has_remote_ui: boolean }
export function observable<T>(initial: T) {
  let value = initial
  const listeners = new Set<() => void>()
  return { getSnapshot: () => value, subscribe(fn: () => void) { listeners.add(fn); return () => {listeners.delete(fn)} }, set(next: T) { if (Object.is(value, next)) return; value = next; for (const fn of listeners) fn() } }
}
export const source = observable<View | undefined>(undefined)
export const notices = observable({transient: '', storage: ''})
export const setNotice = (message: string) => notices.set(updateNotice(notices.getSnapshot(), 'transient', message))
export const setStorageNotice = (message: string) => notices.set(updateNotice(notices.getSnapshot(), 'storage', message))
export const selection = observable('main')
export type SettingsSection='models'|'appearance'|'plugins'|'advanced'
export const settingsPanel=observable<SettingsSection|undefined>(undefined)
export const advancedSettings=observable<string|undefined>(undefined)
export const settingsMounted=observable(false)
type RegisteredSettingsState = {draft:ReturnType<typeof import('./runtime/settings-draft.js').createSettingsDraft>}
  | {catalog:{namespace?:string;ticket?:string;error?:string;page?:{namespaces:string[];next:unknown}};revision?:string;command(value:Record<string,unknown>):Promise<unknown>}
export const registeredSettings=observable<RegisteredSettingsState|undefined>(undefined)
export const connectionStatus=observable({phase:'Disconnected',native:false,reconnect:false,busy:false,closing:false,developmentHttp:false})
export const useView = bindSnapshotSelector(source)
export const useSelected = bindSnapshotSelector(selection)
export const publish = source.set
export const selectSurface = selection.set
type Command = Record<string, unknown>
interface Actions { disconnect():Promise<unknown>; restoreDrafts():Promise<void>; dismissSettings():void; resources():ResourceOwner|undefined; command(command: Command): Promise<unknown>; open(command: Command): Promise<unknown>; select(key: string): void; closeSurface(key: string): Promise<void>; addSurface(key: string): Promise<void>; call(method: string, payload: unknown): Promise<unknown> }
let actions: Actions | undefined
export function installActions(value: Actions) { actions = value }
export const input = {
  disconnect() { if (!actions) return Promise.reject(new Error("Application is starting")); return actions.disconnect() },
  restoreDrafts() { return actions?.restoreDrafts() ?? Promise.reject(new Error('Application is starting')) },
  resourceOwner() { return actions?.resources() },
  dismissSettings() { actions?.dismissSettings();settingsPanel.set(undefined) },
  automationArtifact(value: unknown): Promise<string> { return actions ? actions.call("automation_artifact",JSON.stringify(value)) as Promise<string> : Promise.reject(new Error("Application is starting")) },
  directory(value: unknown): Promise<string> { return actions ? actions.call("directory_input",JSON.stringify(value)) as Promise<string> : Promise.reject(new Error("Application is starting")) },
  command(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.command(value) },
  open(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.open(value) },
  select(key: string) { actions?.select(key) },
  close(key: string) { return actions?.closeSurface(key) ?? Promise.reject(new Error('Application is starting')) },
  add(key: string) { return actions?.addSurface(key) ?? Promise.reject(new Error('Application is starting')) },
}
export function report(error: unknown) { setNotice(error instanceof Error ? error.message : String(error)) }
export async function run(work: () => unknown) { try { await work(); return true } catch (error) { report(error); return false } }
