import type {ExecutionLocation} from '../bridge.ts';
interface SourceRef {seq: string; field: {kind: string; index?: number}}

export type Command = (value: Record<string, unknown>) => Promise<unknown>;
// Presentation projections of gui/details.rs and the approval/question protocols.
// Rust owns validation; these types describe the fields consumed by React.
export interface ImageInterest {result: Promise<string | undefined>; release(): void}
export interface ImageData {
  owner: object;
  ticket: string;
  media: {width: number; height: number; bytes: number};
  acquire(): ImageInterest;
}
interface InteractionBase {pane: string; generation: string; ticket: string}
export type InteractionData = InteractionBase & (
  {kind: 'approval'; request: {id: string; subject: {session_id: string}; action: string; reason: string; review?: {
    arguments: unknown; cwd: string; sandbox: string;
    execution?: {binding?: {location: ExecutionLocation}; workspace?: string};
  } | null}}
  | {kind: 'question'; request: {id: string; session_id: string; questions: {prompt: string; options: string[]}[];
    review?: {binding: unknown; choices: {id: string; label: string}[]} | null}}
);
export interface SourceData {
  owner: object;
  ticket: string; source: SourceRef; error?: string | null;
  window?: {start: number; end: number; more: boolean; text: string} | null;
  media?: ImageData['media']|null; acquire: ImageData['acquire'];
}
export type Call = (method:string, payload:string) => Promise<unknown>;
export interface UiDetail {
  ticket:string; binding:unknown; error:string|null; busy:boolean;
  model:{standard_view:{title:string}|null;actions:{name:string}[]}|null;
}
export interface RemoteCatalog {
  ticket:string;error:string|null;
  page:{entries:{bundle:string;surface:string;title:string}[];next:unknown}|null;
}
export interface BlockSources {pane:string;generation:string;ticket:string;start:number;total:number;page:SourceRef[]}
export interface PanelDetails {
  image_detail:Omit<ImageData,'owner'|'acquire'>|null;
  ui_detail:UiDetail|null;remote_ui_catalog:RemoteCatalog|null;
  block_sources:BlockSources|null;source_detail:Omit<SourceData,'owner'|'media'|'acquire'>|null;
  source_media:ImageData['media']|null;detail:InteractionData|null;
}
export type ResourceContent =
  {kind: 'image'; data: ImageData}
  | {kind: 'interaction'; data: InteractionData}
  | {kind: 'source'; data: SourceData}
  | {kind: 'ui'; data: UiDetail}
  | {kind: 'extensions'; data: RemoteCatalog}
  | {kind: 'block-sources'; data: BlockSources};
export type ResourceModel = {command:Command;call:Call} & ResourceContent;
