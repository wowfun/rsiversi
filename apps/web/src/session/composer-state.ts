import type { Model } from "../bridge.ts";
import type {
  ProjectionActions,
  SessionProjections,
} from "./projection-types.ts";
import type { MediaLimits } from "../presentation-types.ts";
export interface Media {
  id: string;
  width: number;
  height: number;
  bytes: number;
}
interface Pending {
  kind: string;
  id: string;
  phase: string;
  captures: {target: {session_id: string; header_sha256: string}; snapshot: {capture_id: string; sha256: string; byte_len: number}}[];
  images?: number;
}
export interface ComposerView {
  generation?: string;
  bindingInput?: boolean;
  completionsAvailable?: boolean;
  access?: string;
  accessDisabled?: boolean;
  workspace?: string;
  text: string;
  command?: {name:string;hint:string;end:number};
  disabled: boolean;
  sendDisabled: boolean;
  sendBusy: boolean;
  sendLabel: string;
  alternative?: { label: string };
  alternativeDisabled: boolean;
  exporting?: boolean;
  bindingError?: string;
  storageNotice?: string;
  discardConfirmed: boolean;
  discarding?: boolean;
  failure?: { message: string; resolve(saved: boolean): Promise<void> };
  dirty?: boolean;
  pending?: Pending;
  deliveryError?: string;
  cleanups: { id: string; message: string; retry(): Promise<unknown> }[];
  images: Media[];
  importingImages?: {files: File[]; next: number};
  references: { id: string; label: string }[];
  referenceDisabled: boolean;
  referenceRemovalDisabled: boolean;
  fileDisabled: boolean;
  imageDisabled: boolean;
  mediaLimits?: MediaLimits;
}
export interface ComposerOwner extends ProjectionActions {
  input: HTMLTextAreaElement;
  closeCompletions(): void;
  imageInput: HTMLInputElement;
  upload(files: File[]): Promise<void>;
  showFilePicker(): unknown;
  showHistoryPicker(): unknown;
  showReferencePicker(): unknown;
  exportSession(args: string): unknown;
  prepareComposer(): Promise<boolean>;
  clearCommand(): void;
  index: string;
  selectWorkspace(id: string): Promise<unknown> | void;
  stagedPreset?: string;
  selectPreset(id: string): Promise<unknown> | void;
  stopBinding?: unknown;
  composerView?: ComposerView;
  switching?: boolean;
  discardSavedDrafts(): Promise<void>;
  cancelPrepared(): Promise<unknown>;
  reconcile(mode: string): Promise<unknown>;
  moveImage(index: number, to: number | null): void;
  previewImage(index: number): Promise<unknown>;
  acquireDraftImage(media: Media, generation: string): import("../details/model.ts").ImageInterest;
  removeReference(id: string): Promise<unknown>;
  previewReference(id: string): void;
}
export interface ComposerProjection extends SessionProjections {
  model: Model;
  model_command?: { pending?: unknown };
  effort_profile?: { default?: string; supported: string[] };
  reasoning_effort?: string;
}
