import type {Command, InteractionData} from './model.ts';
import { QuestionForm } from "./question-form.tsx";
import { createInteraction, interactionKey } from "../runtime/interaction.js";
import {
  Fragment,
  useMemo,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  approvalFields,
  approvalArguments,
  approvalReason,
  approvalJson,
} from "../../approval-review.js";
export function InteractionDetail({ data, command, session }: {data: InteractionData; command: Command; session: string | null}) {
  const base = {pane: data.pane, generation: data.generation, id: data.request.id};
  const [raw, setRaw] = useState(false),
    feedback = useRef<HTMLTextAreaElement>(null);
  const binding = {command, ticket: data.ticket, refreshCommand: {
    action: "inspect_interaction", ...base,
    owner: data.kind === "approval" ? data.request.subject.session_id : data.request.session_id,
  }};
  const identity = interactionKey(data);
  const composition = useRef<"idle" | "active" | "ended">("idle");
  const state = useMemo(() => createInteraction(binding), [identity]);
  useLayoutEffect(() => state.update(binding), [state, data, command]);
  useSyncExternalStore(state.subscribe, state.getSnapshot);
  const { disabled, pending, refreshing, error } = state,
    decide = state.decide;
  const footer = (error || pending || refreshing || state.refreshed) && (
    <>
      <p className="error" role="alert">
        {error}
      </p>
      <button
        type="button"
        className="quiet"
        disabled={pending || refreshing}
        onClick={() => void state.refresh()}
      >
        Refresh request
      </button>
    </>
  );
  if (data.kind === "question" && data.request.review) {
    const request = data.request;
    const answer = (choice: {id: string}) => {
      const text = feedback.current?.value ?? "";
      if (new TextEncoder().encode(text).length > 4096) {
        state.report("Feedback must fit within 4 KiB.");
        return;
      }
      void decide({
        action: "answer",
        ...base,
        answers: [],
        review: {
          binding: request.review!.binding,
          choice_id: choice.id,
          feedback: text || null,
        },
      });
    };
    return (
      <form
        className="question-review"
        aria-busy={pending || refreshing}
        onSubmit={(event) => event.preventDefault()}
      >
        <div className="approval-status">Review requested</div>
        <div className="question-scroll">
          <pre className="review-plan" tabIndex={0}>{request.questions[0].prompt}</pre>
          <label>
            Optional review feedback
            <textarea
              ref={feedback}
              rows={3}
              aria-label="Optional review feedback"
              disabled={disabled}
            />
          </label>
        </div>
        <div className="actions">
          {request.review!.choices.map((choice) => (
            <button
              type="button"
              className={choice.id === "approve_execute" ? "primary" : undefined}
              key={choice.id}
              disabled={disabled}
              onClick={() => answer(choice)}
            >
              {choice.label}
            </button>
          ))}
        </div>
        {footer}
      </form>
    );
  }
  if (data.kind === "question") {
    return <QuestionForm request={data.request} session={session} disabled={disabled} busy={pending || refreshing}
      submit={answers => { void decide({action: "answer", ...base, answers}); }}
      footer={footer} />;
  }
  const request = data.request;
  const evidence = approvalFields(request.review);
  const answer = (allow: boolean) => {
    if (!state.disabled)
      void decide({ action: "approve", ...base, owner: request.subject.session_id, allow });
  };
  return (
    <form
      className="approval-review"
      aria-busy={pending || refreshing}
      onSubmit={(event) => event.preventDefault()}
      onCompositionStartCapture={() => { composition.current = "active"; }}
      onCompositionEndCapture={() => { composition.current = "ended"; }}
      onKeyUpCapture={() => {
        if (composition.current === "ended") composition.current = "idle";
      }}
      onKeyDown={(event) => {
        const target = event.target as Element;
        if (event.defaultPrevented || !event.currentTarget.contains(document.activeElement)
          || (event.key !== "Enter" && event.key !== "Escape")
          || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey
          || target.closest('input, textarea, select, [contenteditable="true"], [contenteditable=""]')
          || (event.key === "Enter" && target.closest('button, summary, a[href], [role="button"]'))) return;
        event.preventDefault();
        event.stopPropagation();
        if (!event.repeat && composition.current === "idle" && !event.nativeEvent.isComposing
          && event.nativeEvent.keyCode !== 229) answer(event.key === "Enter");
      }}
    >
      <div className="approval-status">Waiting for approval</div>
      <div className="approval-scroll" role="group" aria-label="Approval details" tabIndex={0}>
        <h2 className="approval-action">{request.action}</h2>
        <p className="approval-reason">{approvalReason(request.reason)}</p>
        {request.review && (
          <>
            <h3>Prepared arguments</h3>
            {approvalArguments(request.review.arguments).map(
              ([label, value], index) => (
                <section className="approval-argument" key={index}>
                  {label && <strong>{label}</strong>}
                  <pre tabIndex={0}>{value}</pre>
                </section>
              ),
            )}
          </>
        )}
        {evidence.length > 0 && (
          <details>
            <summary>Execution details · {evidence[0][1]}</summary>
            <dl className="approval-evidence">
              {evidence.map(([label, value]) => (
                <Fragment key={label}><dt>{label}</dt><dd>{value}</dd></Fragment>
              ))}
            </dl>
          </details>
        )}
        <details
          className="approval-raw"
          onToggle={(event) => {
            if (event.currentTarget.open) setRaw(true);
          }}
        >
          <summary>Raw request</summary>
          {raw && <pre tabIndex={0}>{approvalJson(request)}</pre>}
        </details>
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      <div className="actions">
        <button type="button" disabled={disabled} onClick={() => answer(false)}>
          Deny
        </button>
        <button type="button" className="primary" disabled={disabled} onClick={() => answer(true)}>
          Allow once
        </button>
        {(error || pending || refreshing || state.refreshed) && <button type="button" className="quiet" disabled={pending || refreshing} onClick={() => void state.refresh()}>
          Refresh request
        </button>}
      </div>
    </form>
  );
}
