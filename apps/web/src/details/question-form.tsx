import {useLayoutEffect, useRef, useState, type ReactNode} from "react";
import type {InteractionData} from "./model.ts";
import {questionKey, readQuestionProgress, writeQuestionProgress, useQuestionScope, type DraftAnswer, type QuestionProgress} from "./question-progress.ts";
import {IconChevronDownOutlineRegular, IconChevronUpOutlineRegular} from "../../vendor/dsh/dockkit-primitives/icons/index.tsx";

type Request = Extract<InteractionData, {kind: "question"}>["request"];
const recommendation = /\s*(?:\((?:recommended|推荐)\)|（(?:recommended|推荐)）)\s*$/i;

interface FormProps {
  request: Request;
  disabled: boolean;
  busy: boolean;
  submit(answers: string[]): void;
  footer: ReactNode;
}
export function QuestionForm({session, ...props}: FormProps & {session: string | null}) {
  const scope = useQuestionScope(session);
  const key = scope && questionKey(scope, props.request.session_id, props.request.id);
  return <QuestionSteps key={JSON.stringify([key, props.request.turn_id])} storageKey={key} {...props} />;
}

function QuestionSteps({request, disabled, busy, submit, footer, storageKey}: FormProps & {storageKey?: string}) {
  const [progress, setProgress] = useState(() => readQuestionProgress(storageKey, request));
  const [collapsed, setCollapsed] = useState(false);
  const [saveFailed, showSaveFailure] = useState(false);
  const unsaved = useRef<QuestionProgress>();
  const timer = useRef<ReturnType<typeof setTimeout>>();
  function flush(notify: boolean) {
    clearTimeout(timer.current);
    if (!storageKey || !unsaved.current) return;
    try {
      writeQuestionProgress(storageKey, request.turn_id, unsaved.current);
      unsaved.current = undefined;
      if (notify) showSaveFailure(false);
    } catch { if (notify) showSaveFailure(true); }
  }
  useLayoutEffect(() => {
    const save = () => flush(false);
    window.addEventListener("pagehide", save);
    return () => {window.removeEventListener("pagehide", save); save();};
  }, [storageKey]);
  function update(next: QuestionProgress) {
    setProgress(next);
    if (!storageKey) return;
    unsaved.current = next;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => flush(true), 150);
  }
  const index = Math.min(progress.index, request.questions.length - 1);
  const question = request.questions[index];
  const heading = useRef<HTMLHeadingElement>(null);
  const previous = useRef(index);
  useLayoutEffect(() => {
    if (index !== previous.current) heading.current?.focus({preventScroll: true});
    previous.current = index;
  }, [index]);
  function draft(i: number) {
    const item = request.questions[i];
    const first = item.options[0];
    const saved = progress.drafts[i];
    if (saved && (!saved.option || item.options.includes(saved.option))) return saved;
    return {option: first && recommendation.test(first) ? first : undefined, text: ""};
  }
  const current = draft(index);
  function move(position: number) { update({...progress, index: position}); }
  function setAnswer(answer: DraftAnswer, position = index) {
    const drafts = progress.drafts.slice();
    drafts[index] = answer;
    update({index: position, drafts});
  }
  function value(i: number) {
    const answer = draft(i);
    return answer.option ?? answer.text.trim();
  }
  const last = index === request.questions.length - 1;
  function advance() {
    if (disabled || !value(index)) return;
    if (!last) { move(index + 1); return; }
    const answers = request.questions.map((_, i) => value(i));
    const missing = answers.indexOf("");
    if (missing >= 0) move(missing);
    else submit(answers);
  }
  return (
    <form className="question-review ordinary-question" aria-label={question.prompt} aria-busy={busy}
      onSubmit={event => {event.preventDefault(); advance();}}>
      <div className="question-header">
        {collapsed && <h2>{question.prompt}</h2>}
        <button type="button" aria-expanded={!collapsed} aria-label={collapsed ? "Expand the question card" : "Collapse the question card"}
          onClick={() => setCollapsed(!collapsed)}>
          {collapsed ? <IconChevronUpOutlineRegular size={18}/> : <IconChevronDownOutlineRegular size={18}/>}
        </button>
      </div>
      {!collapsed && <>
      <div className="question-scroll">
        <h2 ref={heading} tabIndex={-1}>{question.prompt}</h2>
        {question.options.length > 0 && (
          <div role="radiogroup" aria-label={question.prompt}>
            {question.options.map((option, i) => (
              <label key={i} className="question-choice">
                <input type="radio" name={question.id} checked={current.option === option}
                  disabled={disabled} aria-label={option.replace(recommendation, "")}
                  onChange={() => {
                    setAnswer({option, text: ""}, last ? index : index + 1);
                  }} />
                <span>{option.replace(recommendation, "")}</span>
                {recommendation.test(option) && <small>Recommended</small>}
              </label>
            ))}
          </div>
        )}
        <textarea rows={2} aria-label={question.prompt}
          placeholder={question.options.length ? "Write another answer" : "Your answer"}
          disabled={disabled} value={current.text}
          onChange={event => setAnswer({text: event.target.value})}
          onKeyDown={event => {
            if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
            event.preventDefault();
            if (!event.repeat) advance();
          }} />
      </div>
      <div className="actions">
        {request.questions.length > 1 && <>
          <span>{index + 1} / {request.questions.length}</span>
          <button type="button" disabled={disabled || index === 0} onClick={() => move(index - 1)}>Previous</button>
        </>}
        <button className="primary" type="submit" disabled={disabled || !value(index)}>{last ? "Send answers" : "Next"}</button>
      </div>
      </>}
      {saveFailed && <p role="status">This browser could not save your question progress.</p>}
      {footer}
    </form>
  );
}
