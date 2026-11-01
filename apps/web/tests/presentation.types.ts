import type {Surface, ExternalPermission, TranscriptBlock} from '../src/presentation-types.ts';
import type {View} from '../src/bridge.ts';
declare const view:View;
const maxUpload:number=view.media_limits.upload_bytes;
const hasMedia:boolean=view.has_media;
const panelRevision:string|undefined=view.panels[0]?.open_revision;
const settingsTicket:string|undefined=view.settings?.ticket;
const nextNamespace:string|null|undefined=view.settings_catalog?.page?.next;
const settingsRevision:string=view.settings_revision;
const defaultSandbox:import('../src/presentation-types.ts').SandboxMode|undefined=view.setup?.agent?.sandbox;
const detail=view.panels[0]?.detail;
if (detail?.kind === 'approval') {
  const session:string=detail.request.subject.session_id;
  void session;
  // @ts-expect-error Approval requests do not carry question lists.
  detail.request.questions;
}
const uiAction:string|undefined=view.panels[0]?.ui_detail?.model?.actions[0]?.name;
const sourceSequence:string|undefined=view.panels[0]?.source_detail?.source.seq;
void [defaultSandbox,uiAction,sourceSequence];
void [maxUpload,hasMedia,panelRevision,settingsTicket,nextNamespace,settingsRevision];
// @ts-expect-error Media bounds are numeric wire values.
const invalidLimits:View['media_limits']={images:'8'};
void invalidLimits;
declare const surface: Surface;
if (surface.kind === 'external') {
  const selection:string=surface.selection;
  void selection;
  const permission: ExternalPermission | undefined = surface.external.observed.permissions[0];
  permission?.options[0]?.id;
  // @ts-expect-error External observations do not have native session authority.
  surface.session;
} else {
  const selection:string=surface.selection;
  const creationWorkspace:string|undefined=surface.creation?.workspace_id;
  const active:string|null=surface.active;
  const uiRevision:string=surface.ui_revision;
  const sandbox:'read-only'|'workspace-write'|'danger-full-access'=surface.sandbox;
  const completionSequence:string|undefined=surface.completions?.sequence;
  void completionSequence;
  // @ts-expect-error Completion revisions use decimal wire strings.
  const invalidCompletion:NonNullable<typeof surface.completions>={query:'',sequence:1,entries:[],notice:null};
  void invalidCompletion;
  void [selection,creationWorkspace,active,uiRevision,sandbox];
  const block: TranscriptBlock | undefined = surface.transcript.blocks[0];
  const opaque: unknown = block?.tool?.result;
  void opaque;
  // @ts-expect-error A native surface does not carry ACP observation state.
  surface.external;
}
