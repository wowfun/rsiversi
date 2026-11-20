import {modalSelector} from "../../vendor/dsh/dockkit-primitives/useModalLayer.ts";
import {run} from "../bridge.ts";
import type {ComposerOwner} from "./composer-state.ts";

interface Target {
  input: HTMLTextAreaElement;
  owner: ComposerOwner;
  showDrag(active: boolean): void;
}

const targets = new Set<Target>();
let lastFocused: Target | undefined;
let dragTarget: Target | undefined;
let depth = 0;
let listeners: AbortController | undefined;

function destination(event: Event, pasting = false) {
  const origin = event.composedPath().find(node => node instanceof Element);
  const editable = pasting && origin?.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"])');
  const foreground = Array.from(document.querySelectorAll('dialog[open], ' + modalSelector))
    .filter(element => element.checkVisibility({visibilityProperty:true})).at(-1);
  let main: Target | undefined;
  let focused: Target | undefined;
  for (const target of targets) {
    const {input, owner} = target;
    if (!owner.composerView?.mediaLimits || (pasting && owner.composerView.imageDisabled) ||
        (editable && editable !== input) || (foreground && !foreground.contains(input)) ||
        input.closest('[inert]') || !input.checkVisibility({visibilityProperty:true})) continue;
    if (editable === input || (!pasting && origin && input.closest(".pane")?.contains(origin))) return target;
    if (target === lastFocused) focused = target;
    if (owner.index === "main") main = target;
  }
  return focused ?? main;
}

function paste(event: ClipboardEvent) {
  if (event.defaultPrevented || !event.clipboardData) return;
  const files: File[] = [];
  for (const item of event.clipboardData.items) {
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const file = item.getAsFile();
    if (file) files.push(file);
  }
  if (!files.length) return;
  const target = destination(event, true);
  if (!target) return;
  event.preventDefault();
  const text = event.clipboardData.getData("text/plain");
  if (text && event.composedPath()[0] === target.input) {
    const input = target.input;
    input.setRangeText(text, input.selectionStart, input.selectionEnd, "end");
    input.dispatchEvent(new InputEvent("input", {bubbles:true, inputType:"insertFromPaste", data:text}));
  }
  void run(() => target.owner.upload(files));
}

function imageDrag(event: DragEvent) {
  return event.dataTransfer && Array.from(event.dataTransfer.items).some(item => item.kind === "file" && item.type.startsWith("image/"));
}

function showDrag(target?: Target) {
  if (target === dragTarget) return;
  dragTarget?.showDrag(false);
  dragTarget = target;
  target?.showDrag(true);
}

function resetDrag() {
  depth = 0;
  showDrag();
}

function install() {
  const controller = new AbortController();
  const options = {signal:controller.signal};
  document.addEventListener("paste", paste, options);
  document.addEventListener("dragenter", event => {
    if (!imageDrag(event)) return;
    const target = destination(event);
    if (!target) return;
    event.preventDefault();
    depth++;
    showDrag(target);
  }, options);
  document.addEventListener("dragover", event => {
    if (!imageDrag(event)) return;
    const target = dragTarget ?? destination(event);
    if (!target) return;
    event.preventDefault();
    event.dataTransfer!.dropEffect = target.owner.composerView?.imageDisabled ? "none" : "copy";
  }, options);
  document.addEventListener("dragleave", event => {
    if (!depth) return;
    const leftPage = (event.target === document.documentElement || event.target === document.body) &&
      (event.clientX <= 0 || event.clientY <= 0 || event.clientX >= innerWidth || event.clientY >= innerHeight);
    if (--depth === 0 || leftPage) resetDrag();
  }, options);
  document.addEventListener("drop", event => {
    resetDrag();
    if (event.defaultPrevented || !event.dataTransfer) return;
    const files = Array.from(event.dataTransfer.files).filter(file => file.type.startsWith("image/"));
    if (!files.length) return;
    const target = destination(event);
    if (!target) return;
    event.preventDefault();
    if (!target.owner.composerView?.imageDisabled) void run(() => target.owner.upload(files));
  }, options);
  window.addEventListener("dragend", resetDrag, options);
  return controller;
}

export function bindImageIntake(input: HTMLTextAreaElement, owner: ComposerOwner, showDrag: Target["showDrag"]) {
  const form = input.form!;
  const target = {input, owner, showDrag};
  const focus = () => { lastFocused = target; };
  targets.add(target);
  listeners ??= install();
  if (form.contains(document.activeElement)) focus();
  form.addEventListener("focusin", focus);
  return () => {
    form.removeEventListener("focusin", focus);
    targets.delete(target);
    if (lastFocused === target) lastFocused = undefined;
    if (dragTarget === target) resetDrag();
    if (!targets.size) { listeners?.abort(); listeners = undefined; }
  };
}
