import {useSyncExternalStore} from "react";
import {presentationIdentity} from "../presentation.tsx";
import {assertDraftStorageActive, questionProgressPrefix, removeQuestionDrafts} from "../../draft-databases.js";
import type {ObservedRequest} from "./model.ts";

type Request = Extract<ObservedRequest, {kind: "question"}>["request"];
export type DraftAnswer = {option?: string; text: string};
export type QuestionProgress = {index: number; drafts: (DraftAnswer | null)[]};
const maximumCharacters = 128 * 1024;

export function useQuestionScope(session?: string | null) {
  const identity = useSyncExternalStore(presentationIdentity.subscribe, presentationIdentity.getSnapshot);
  return identity && session ? questionProgressPrefix + JSON.stringify([identity, session]) + "/" : undefined;
}

export function questionKey(scope: string, owner: string, id: string) {
  return scope + JSON.stringify([owner, id]);
}

export function readQuestionProgress(key: string | undefined, request: Request): QuestionProgress {
  const empty = {index: 0, drafts: request.questions.map(() => null)};
  if (!key) return empty;
  try {
    const raw = localStorage.getItem(key);
    if (!raw || raw.length > maximumCharacters) return empty;
    const progress = JSON.parse(raw);
    if (progress.turn !== request.turn_id || !Number.isInteger(progress.index) || progress.index < 0 || progress.index >= request.questions.length ||
        !Array.isArray(progress.drafts) || progress.drafts.length !== request.questions.length) return empty;
    if (!progress.drafts.every((answer: DraftAnswer | null, i: number) => answer === null ||
      (answer && typeof answer.text === "string" && (answer.option === undefined ||
        (answer.text === "" && request.questions[i].options.includes(answer.option)))))) return empty;
    return {index: progress.index, drafts: progress.drafts};
  } catch { return empty; }
}

export function writeQuestionProgress(key: string, turn: string, progress: QuestionProgress) {
  assertDraftStorageActive();
  const raw = JSON.stringify({turn, ...progress});
  if (raw.length > maximumCharacters) throw new Error("Question progress exceeds the storage limit");
  localStorage.setItem(key, raw);
}

export function pruneQuestionProgress(scope: string, pending: readonly {kind: string; owner: string; id: string}[]) {
  try {
    const keep = new Set(pending.filter(item => item.kind === "question").map(item => questionKey(scope, item.owner, item.id)));
    removeQuestionDrafts(scope, keep);
  } catch { /* Question progress is optional when site storage is unavailable. */ }
}
