import type { ExternalOwner } from "./external.tsx";
import type { HistoryPickerState } from "./history-picker.tsx";
import type { FilePickerState } from "./file-picker.tsx";
import type { SavedDraftsState } from "./saved-drafts.tsx";
import type { ReferencePickerState } from "./reference-picker.tsx";
import type { QueueEditorState } from "./queue-editor.tsx";
import type { ComposerOwner, ComposerProjection } from "./composer-state.ts";
import type { Model } from "../bridge.ts";
import type {MetricsRead} from './metrics.tsx';
import type {
  InlineCard,
  TranscriptOwner,
  TranscriptView,
} from "./transcript.tsx";
import { observable } from "../bridge.ts";

export interface SessionSurfaceOwner extends TranscriptOwner, ComposerOwner {
  subscribe(listener: () => void): () => void;
  getSnapshot(): number;
  stopBinding?: unknown;
  queueDialog?: QueueEditorState;
  referenceDialog?: ReferencePickerState;
  recoveryView?: SavedDraftsState;
  fileDialog?: FilePickerState;
  historyDialog?: HistoryPickerState;
  index: string;
  kind?: "native";
  staged?: {
    models: Model[];
    submitKey: string;
    data:
      | (ComposerProjection & {
          notice?: string;
          session: string;
          path: string;
          workspace: string;
          historical?: boolean;
          history_more?: boolean;
          transcript: TranscriptView;
          generation: string;
          metrics?: MetricsRead | null;
          context_capacity_tokens?: number | null;
          ui_cards?: boolean;
          inline?: Record<string, InlineCard>;
        })
      | null;
  };
  node: HTMLElement;
  mount(elements: Record<string, HTMLElement>): void;
  unmount(): void;
  applyFrame(): void;
}

export const sessionSurfaces = observable<
  readonly (SessionSurfaceOwner | ExternalOwner)[]
>([]);
