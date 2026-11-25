import type { SshView } from "./ssh-targets.tsx"
import { bindSnapshotSelector } from '../vendor/dsh/renderer/bind.ts'
import type { LeafView } from './profile-leaves.tsx'
import type {AutomationView} from './automation.tsx'
import type {ResourceOwner} from './resource-owner.mjs'
import {updateNotice} from '../notice-state.js'

export interface Model { deployment: string; model: string }
export type ExecutionLocation = {kind:'local'} | {kind:'ssh';target:string}
export interface Coordinates {location:ExecutionLocation;path:string}
export interface OrderMember {session:string;group:number;pinned:boolean;partition_revision?:string;archived:boolean;last_activity_ms:string}
export interface ManualOrder {ticket:string;seed:{scope:{kind:'all'}|{kind:'coordinates';coordinates:Coordinates};host_epoch:string;metadata_revision:string;membership:{kind:'too_large'}|{kind:'available';groups:Coordinates[];members:OrderMember[]}};requested:string[];entries:(NavigationEntry|null)[]}
export interface NavigationEntry { session: string; created_at_ms: string; last_activity_ms:string;location:ExecutionLocation; path: string; workspace: string | null; metadata: {title: string | null; archived: boolean; pinned: boolean} }
export interface AttentionPosition {conversation:{kind:'native'|'external';id:string};epoch:string;sequence:string}
export interface AttentionEntry {position:AttentionPosition;reading:{unseen:boolean;manual:boolean;failed:boolean};status:'waiting'|'running'|'unknown'|'unread';targets:({kind:'native';request:{kind:'approval'|'question';turn:string;request:string}}|{kind:'external';generation:string;request:string})[]}
export type WorkspaceFilter = {kind:'all'} | {kind:'registered';id:string} | {kind:'unregistered'}
export interface NavigationGroup {ticket:string;workspace:WorkspaceFilter;entries:NavigationEntry[];more:boolean;scanned:number;continued:boolean;stale:boolean}
export type PinnedEntry = {status:'available';entry:NavigationEntry} | {status:'missing';session:string;metadata:NavigationEntry['metadata']}
export type ArchiveActivity = {kind:'turn'} | {kind:'job'|'subagent';items:{id:string;label:string}[]};
export interface Navigation { can_archive:boolean; order?:ManualOrder|null;newer_activity:boolean;metadata_revision:string; ticket: string; filter: {query: string; archived: boolean|null; workspace: WorkspaceFilter}; pins:PinnedEntry[]; groups:Record<string,NavigationGroup>; entries: NavigationEntry[]; more: boolean; continued:boolean; scanned: number; diagnostic: string | null; attention?:{host_epoch:string;entries:AttentionEntry[];truncated:boolean};attention_notice?:string }
export interface Provider { provider: 'deepseek' | 'openai' | 'openai-compatible'; config: Record<string, unknown> }
export interface Setup { discovery?:{request:{provider:string;endpoint:string;slot:string};models:{id:string;name?:string|null;context_window_tokens?:number|null;max_output_tokens?:number|null}[]}|null; ticket: string; allowed: boolean; credentials_available: boolean; providers_writable: boolean; settings_writable: boolean; agent: {default_model?: Model; default_reasoning_effort?: string | null;sandbox:import('./presentation-types.ts').SandboxMode; permission_default?: {preset: string; sandbox: import('./presentation-types.ts').SandboxMode; approval: 'ask'|'never'}} | null; default_model_description?: {model: Model; profile: {reasoning_efforts: {supported: string[]; default?: string | null}}} | null; presets: {default?: string} | null; providers: {desired_revision: string; applied_revision: string; applying: boolean; diagnostic: string | null; deployments: Provider[]} | null; credential?: {provider: string; slot: string; status: {editable: boolean; availability: {kind: 'configured'|'missing'|'unavailable'; source?: unknown}}}; diagnostic: string | null; receipts: {operation: string; outcome: string; message: string}[] }
import type {Surface, ExternalSnapshot, MediaLimits, SettingsCatalog} from './presentation-types.ts'
import type {PanelEntry} from './resource-hosts.ts'
import type {SettingsProjection} from './runtime/settings-draft.js'
export interface McpCredentialTarget {server:string;reference:{owner:string;slot:string}}
export interface McpServer {id:string;enabled:boolean;transport:'http'|'stdio'|'ssh_stdio';credential:{owner:string;slot:string}|null;epoch:string;last_verified_sha256:string|null;ready:boolean;error:string|null;tools:{name:string;selected:boolean}[]}
export interface McpStatus {settings_pending:boolean;fresh_ready:boolean;fresh_error:string|null;servers:McpServer[]}
export interface PluginsView {actions:string[];mcp_ssh?:import('./mcp-ssh.tsx').McpSshView;leaves?:LeafView;ssh?:SshView;target:{kind:string};guidance:string[];exa_available:boolean;exa_credential:{availability:{kind:string};editable:boolean}|null;exa_notice:string|null;mcp_available:boolean;mcp:McpStatus|null;mcp_notice:string|null;mcp_credential:{target:McpCredentialTarget;availability:{kind:string};editable:boolean}|null;ticket:string;diagnostic:string|null;page:{context:{target:{kind:string};availability:string;source_digest:string|null;preset_source:string|null};desired_revision:string;observed_revision:string;health:string|null;watcher:string|null;offset:number;total:number;next_offset:number|null;plugins:{instance:string;desired_plugin:string|null;enabled:boolean;origin:string;diagnostics:string[];observed:{plugin:string;state:string}|null}[]}|null}
export interface AgentPreset {id:string;name:string|null;description:string|null;is_default:boolean;broken:string|null}
export interface WorkspaceRecord {id:string;coordinates:Coordinates;name?:string|null;created_at_ms?:number|null}
export interface View {
  media_limits:MediaLimits;has_media:boolean;panels:PanelEntry[];
  settings:SettingsProjection|null;settings_catalog:SettingsCatalog|null;settings_revision:string;
  notice?:string; preferences?:{submit_key?:string}; composer_actions?:Record<string,unknown>; automation?:AutomationView|null; appearance?:{theme:"system"|"light"|"dark";content_font_size:number};preference_error?:string; external_catalog?:{endpoints:{id:string;enabled:boolean}[];conversations:ExternalSnapshot[];more:boolean};plugins?: PluginsView; application_surfaces: {bundle:string;title:string;reference:{name:string;[key:string]:unknown}}[]; surfaces: Record<string, Surface | null>; catalog: {presets?:{presets:AgentPreset[]};workspaces: WorkspaceRecord[]; workspaces_more: boolean; workspace_order_seed:{kind:'available';records:WorkspaceRecord[]}|{kind:'too_large'}|null; models: Model[]; models_more: boolean; models_at_start:boolean}; navigation?: Navigation; setup?: Setup; has_remote_ui: boolean }
export function observable<T>(initial: T) {
  let value = initial
  const listeners = new Set<() => void>()
  return { getSnapshot: () => value, subscribe(fn: () => void) { listeners.add(fn); return () => {listeners.delete(fn)} }, set(next: T) { if (Object.is(value, next)) return; value = next; for (const fn of listeners) fn() } }
}
export const source = observable<View | undefined>(undefined)
type CommandWarning = {id: string; text: string; anchor: HTMLElement}
export const notices = observable<{transient:string; storage:string; warning?:CommandWarning}>({transient:'',storage:''})
export const setCommandWarning = (warning?: CommandWarning) => notices.set({...notices.getSnapshot(), warning})
export const setNotice = (message: string) => notices.set(updateNotice(notices.getSnapshot(), 'transient', message))
export const setStorageNotice = (message: string) => notices.set(updateNotice(notices.getSnapshot(), 'storage', message))
export const selection = observable('main')
export const compactNavigation = observable<'drawer' | 'column' | undefined>(undefined)
export function dismissNavigationDrawer() {
  if (compactNavigation.getSnapshot() === 'drawer') compactNavigation.set(undefined)
}
export interface ScheduledGroup {position:{session_id:string};archived:boolean;exhausted:boolean;reminders:{rule:import('./session/scheduled-tasks.tsx').Reminder;executing:boolean}[]}
type CatalogUpdate = {kind:'page';index:number;sessions:ScheduledGroup[]}|{kind:'complete';pages:number}
interface CatalogView {kind:'tasks';groups?:ScheduledGroup[];error?:string}
export const workbenchPage = observable<CatalogView | {kind:'plugins'} | undefined>(undefined)
export const pageKind = () => workbenchPage.getSnapshot()?.kind
export function currentScheduleCatalog() {
  const page = workbenchPage.getSnapshot();
  return page?.kind === 'tasks' ? page : undefined;
}
let catalogId = 0
let catalogOpen = false

function stopCatalogObservation() {
  if (catalogOpen) {
    catalogOpen = false;
    void actions?.call("scheduled_tasks",{action:"close",id:catalogId}).catch(() => {});
  }
}
export function closeWorkbenchPage() {
  stopCatalogObservation();
  workbenchPage.set(undefined);
}
export function openPluginPage() {
  if (pageKind() === 'plugins') return;
  stopCatalogObservation();
  workbenchPage.set({kind:'plugins'});
}

export async function loadScheduleCatalog() {
  const id = ++catalogId;
  catalogOpen = true;
  workbenchPage.set({kind:'tasks'});
  try {
    if (!actions) throw new Error("Application is starting");
    const pages: ScheduledGroup[][] = [];
    let action = "open";
    while (catalogOpen && id === catalogId) {
      const value = await actions.call("scheduled_tasks",{action,id});
      if (!catalogOpen || id !== catalogId) return;
      if (!value) throw new Error("Task catalog connection closed");
      const update: CatalogUpdate = JSON.parse(value as string);
      if (update.kind === "page") pages[update.index] = update.sessions;
      else {
        pages.length = update.pages;
        workbenchPage.set({kind:'tasks',groups:pages.flat()});
      }
      action = "next";
    }
  } catch (reason) {
    if (catalogOpen && id === catalogId) {
      stopCatalogObservation();
      workbenchPage.set({kind:'tasks',error: reason instanceof Error ? reason.message : String(reason)});
    }
  }
}
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
export type {SetupBinding} from './runtime/setup-binding.ts'
export type BackgroundOutput = {stdout:string; stderr:string; truncated:boolean};
interface Actions { captureSetupBinding():import('./runtime/setup-binding.ts').SetupBinding; disconnect():Promise<unknown>; restoreDrafts():Promise<void>; dismissSettings():void; resources():ResourceOwner|undefined; command(command: Command): Promise<unknown>; open(command: Command, startingText?: string): Promise<unknown>; select(key: string): void; closeSurface(key: string): Promise<void>; addSurface(key: string): Promise<void>; call(method: string, payload: unknown, transfer?:Transferable[]): Promise<unknown> }
let actions: Actions | undefined
export function installActions(value: Actions) { actions = value }
export type ArchiveAction = 'archive' | 'stop_and_archive' | 'restore';
export const input = {
  async compactionSummary(pane:string, generation:string, sequence:string, signal:AbortSignal) {
    if (!actions) throw new Error('Application is starting');
    signal.throwIfAborted();
    const {port1,port2} = new MessageChannel();
    const cancel = () => port1.postMessage(null);
    signal.addEventListener('abort',cancel);
    try {
      const result = await actions.call('compaction_summary', {pane,generation,sequence,port:port2}, [port2]);
      return JSON.parse(result as string) as {markdown:import('./session/markdown.tsx').MarkdownEvent[]}|{text:string};
    } finally {
      signal.removeEventListener('abort',cancel);
      port1.close();
      port2.close();
    }
  },
  async archive(session: string, action: ArchiveAction): Promise<ArchiveActivity[]|null> {
    if (!actions) throw new Error('Application is starting');
    return JSON.parse(await actions.call("archive_session", JSON.stringify({session,action})) as string);
  },
  transcriptFile(pane:string,generation:string,kind:"image"|"document",path:string):Promise<[string,Uint8Array<ArrayBuffer>]> {
    if (!actions) return Promise.reject(new Error('Application is starting'));
    return actions.call("transcript_file",JSON.stringify({pane,generation,kind,path})) as Promise<[string,Uint8Array<ArrayBuffer>]>;
  },
  captureSetupBinding() { if (!actions) throw new Error('Application is starting'); return actions.captureSetupBinding() },
  disconnect() { if (!actions) return Promise.reject(new Error("Application is starting")); return actions.disconnect() },
  restoreDrafts() { return actions?.restoreDrafts() ?? Promise.reject(new Error('Application is starting')) },
  resourceOwner() { return actions?.resources() },
  dismissSettings() { actions?.dismissSettings();settingsPanel.set(undefined) },
  async backgroundJob(pane:string, generation:string, action:unknown):Promise<unknown> {
    if (!actions) throw new Error('Application is starting');
    return JSON.parse(await actions.call('background_job',JSON.stringify({pane,generation,action})) as string);
  },
  observeBackgroundJob(pane:string, generation:string, job:{id:string;origin?:string|null},
    update:(value:BackgroundOutput)=>void, failed:(reason:string)=>void) {
    if (!actions) { failed('Application is starting'); return; }
    const {port1, port2} = new MessageChannel();
    let active = true;
    port1.onmessage = ({data}) => {
      if (data.error !== undefined) failed(data.error);
      else update(JSON.parse(data.value) as BackgroundOutput);
    };
    void actions.call('background_output',{pane,source:JSON.stringify({pane,generation,job}),port:port2},[port2])
      .catch(reason => { if (active) failed(String(reason)); port1.close(); });
    return () => { active = false; port1.postMessage(null); port1.close(); };
  },
  async messageFeedback(pane: string, generation: string, message: string, action: unknown) {
    if (!actions) throw new Error('Application is starting');
    const response = await actions.call("message_feedback", JSON.stringify({pane,generation,message,action})) as string;
    return JSON.parse(response) as {conflict:boolean;current:{version:string;rating:'positive'|'negative'}|null};
  },
  automationArtifact(value: unknown): Promise<string> { return actions ? actions.call("automation_artifact",JSON.stringify(value)) as Promise<string> : Promise.reject(new Error("Application is starting")) },
  directory(value: unknown): Promise<string> { return actions ? actions.call("directory_input",JSON.stringify(value)) as Promise<string> : Promise.reject(new Error("Application is starting")) },
  command(value: Command) { if (!actions) return Promise.reject(new Error('Application is starting')); return actions.command(value) },
  async open(value: Command, startingText?: string) { if (!actions) throw new Error('Application is starting'); const result = await actions.open(value, startingText); closeWorkbenchPage(); dismissNavigationDrawer(); return result },
  select(key: string) { actions?.select(key) },
  close(key: string) { return actions?.closeSurface(key) ?? Promise.reject(new Error('Application is starting')) },
  add(key: string) { return actions?.addSurface(key) ?? Promise.reject(new Error('Application is starting')) },
}
export function report(error: unknown) { setNotice(error instanceof Error ? error.message : String(error)) }
export async function run(work: () => unknown) { try { await work(); return true } catch (error) { report(error); return false } }
