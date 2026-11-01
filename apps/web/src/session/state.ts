import type { ExternalOwner } from "./external.tsx";
import type { HistoryPickerState } from "./history-picker.tsx";
import type { FilePickerState } from "./file-picker.tsx";
import type { SavedDraftsState } from "./saved-drafts.tsx";
import type { ReferencePickerState } from "./reference-picker.tsx";
import type { QueueEditorState } from "./queue-editor.tsx";
import type { ComposerOwner } from "./composer-state.ts";
import type { Model } from "../bridge.ts";
import type {NativeSurface} from '../presentation-types.ts';
import type {
  TranscriptOwner,
} from "./transcript.tsx";
import { observable } from "../bridge.ts";

export interface SessionSurfaceOwner extends TranscriptOwner, ComposerOwner {
  subscribe(listener: () => void): () => void;
  getSnapshot(): number;
  queueDialog?: QueueEditorState;
  referenceDialog?: ReferencePickerState;
  recoveryView?: SavedDraftsState;
  fileDialog?: FilePickerState;
  historyDialog?: HistoryPickerState;
  index: string;
  kind?: "native";
  staged?: {
    models: Model[];
    composerActions?: unknown;
    submitKey: string;
    data: NativeSurface | null;
  };
  node: HTMLElement;
  mount(elements: Record<string, HTMLElement>): void;
  unmount(): void;
  dispose(): void;
  applyFrame(): void;
}

export const sessionSurfaces = observable<
  readonly (SessionSurfaceOwner | ExternalOwner)[]
>([]);
