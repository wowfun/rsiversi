import type {Command, InteractionData} from './model.ts';
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
export function InteractionDetail({ data, command }: {data: InteractionData; command: Command}) {
  const base = {pane: data.pane, generation: data.generation, id: data.request.id};
  const [raw, setRaw] = useState(false),
    fields = useRef<(HTMLTextAreaElement | null)[]>([]),
    feedback = useRef<HTMLTextAreaElement>(null);
  const binding = {command, ticket: data.ticket, refreshCommand: {
    action: "inspect_interaction", ...base,
    owner: data.kind === "approval" ? data.request.subject.session_id : data.request.session_id,
  }};
  const identity = interactionKey(data);
  const state = useMemo(() => createInteraction(binding), [identity]);
  useLayoutEffect(() => state.update(binding), [state, data, command]);
  useSyncExternalStore(state.subscribe, state.getSnapshot);
  const { disabled, pending, refreshing, error } = state,
    decide = state.decide;
  const footer = (
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
        aria-busy={pending || refreshing}
        onSubmit={(event) => event.preventDefault()}
      >
        <pre className="review-plan">{request.questions[0].prompt}</pre>
        <label>
          Optional review feedback
          <textarea
            ref={feedback}
            rows={3}
            aria-label="Optional review feedback"
            disabled={disabled}
          />
        </label>
        <div className="actions">
          {request.review!.choices.map((choice) => (
            <button
              type="button"
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
    const request = data.request;
    return (
      <form
        aria-busy={pending || refreshing}
        onSubmit={(event) => {
          event.preventDefault();
          if (!disabled)
            void decide({
              action: "answer",
              ...base,
              answers: fields.current.map((input) => input?.value ?? ""),
            });
        }}
      >
        {request.questions.map((question, index) => (
          <div className="question-field" key={index}>
            <label>{question.prompt}</label>
            <div className="actions">
              {question.options.map((option) => (
                <button
                  type="button"
                  disabled={disabled}
                  key={option}
                  onClick={() => {
                    const input = fields.current[index];
                    if (input) {input.value = option; input.focus();}
                  }}
                >
                  {option}
                </button>
              ))}
            </div>
            <textarea
              ref={(node) => {fields.current[index] = node;}}
              rows={2}
              required
              aria-label={question.prompt}
              disabled={disabled}
            />
          </div>
        ))}
        <button type="submit" className="primary" disabled={disabled}>
          Send answers
        </button>
        {footer}
      </form>
    );
  }
  const request = data.request;
  const evidence = approvalFields(request.review);
  return (
    <form
      className="approval-review"
      aria-busy={pending || refreshing}
      onSubmit={(event) => event.preventDefault()}
    >
      <h2 className="approval-action">{request.action}</h2>
      <p className="approval-reason">{approvalReason(request.reason)}</p>
      {!!evidence.length && (
        <dl className="approval-evidence">
          {evidence.map(([label, value]) => (
            <Fragment key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </Fragment>
          ))}
        </dl>
      )}
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
      <details
        className="approval-raw"
        onToggle={(event) => {
          if (event.currentTarget.open) setRaw(true);
        }}
      >
        <summary>Raw request</summary>
        {raw && <pre tabIndex={0}>{approvalJson(request)}</pre>}
      </details>
      <div className="actions">
        <button
          type="button"
          className="primary"
          disabled={disabled}
          onClick={() =>
            void decide({
              action: "approve",
              ...base,
              owner: request.subject.session_id,
              allow: false,
            })
          }
        >
          Deny
        </button>
        <button
          type="button"
          disabled={disabled}
          onClick={() =>
            void decide({
              action: "approve",
              ...base,
              owner: request.subject.session_id,
              allow: true,
            })
          }
        >
          Allow once
        </button>
      </div>
      {footer}
    </form>
  );
}
