import type { Model } from "../bridge.ts";
import type {
  ProjectionActions,
  SessionProjections,
} from "./projection-types.ts";
interface Media {
  width: number;
  height: number;
  bytes: number;
}
interface Pending {
  kind: string;
  id: string;
  phase: string;
  references?: number;
  images?: number;
}
export interface ComposerView {
  access?: string;
  accessDisabled?: boolean;
  workspace?: string;
  text: string;
  disabled: boolean;
  sendDisabled: boolean;
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
  cleanups: { id: string; message: string; retry(): Promise<unknown> }[];
  images: Media[];
  references: { id: string; label: string }[];
  referenceDisabled: boolean;
  fileDisabled: boolean;
  imageDisabled: boolean;
  mediaEnabled: boolean;
}
export interface ComposerOwner extends ProjectionActions {
  prepareComposer(): Promise<boolean>;
  setAccess(mode: string): void;
  selectCommand(name: string): void;
  index: string;
  selectWorkspace(id: string): Promise<unknown> | void;
  stopBinding?: unknown;
  composerView?: ComposerView;
  switching?: boolean;
  discardSavedDrafts(): Promise<void>;
  cancelPrepared(): Promise<unknown>;
  reconcile(mode: string): Promise<unknown>;
  moveImage(index: number, to: number | null): void;
  previewImage(index: number): Promise<unknown>;
  removeReference(id: string): Promise<unknown>;
  previewReference(id: string): void;
}
export interface ComposerProjection extends SessionProjections {
  model: Model;
  model_command?: { pending?: unknown };
  effort_profile?: { default?: string; supported: string[] };
  reasoning_effort?: string;
}
