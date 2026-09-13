#!/usr/bin/env python3
"""Fixed Session API Goal attempts graded by coding.py's external immutable oracle."""
import argparse
import difflib
import hashlib
import json
from pathlib import Path
import re
import shutil
import tempfile
import time

import coding
from session_fixture import BOUNDARY_MARKER, PRESSURE_TEXT, SUMMARY_FRAME, SUMMARY_TEXT, Provider

ROUNDS = 2
TASK_SECONDS = 480


class EvidenceCredentialError(RuntimeError):
    pass


def redacted(value, key):
    if not key:
        return value
    if isinstance(value, str):
        for token in sorted({key, json.dumps(key)[1:-1], json.dumps(key, ensure_ascii=False)[1:-1]}, key=len, reverse=True):
            value = value.replace(token, "[REDACTED]")
        return value
    if isinstance(value, list):
        return [redacted(item, key) for item in value]
    if isinstance(value, dict):
        return {redacted(name, key): redacted(item, key) for name, item in value.items()}
    return value


def persist(path, value, key):
    if redacted(value, key) != value:
        raise EvidenceCredentialError("credential appeared in evidence; refusing to persist it")
    path.write_text(value if isinstance(value, str) else json.dumps(value, indent=2))


def turn_budget(task):
    return dict(maximum_elapsed_ms=120000, maximum_provider_attempts=16,
                maximum_tool_calls=48 // (ROUNDS * len(task["prompts"])),
                maximum_generated_records=65536, maximum_generated_record_bytes=67108864)


def limits(task, budget):
    rounds = ROUNDS * len(task["prompts"])
    return dict(task_attempts=1, task_wall_seconds=TASK_SECONDS, automatic_rounds_per_stage=ROUNDS,
                maximum_parent_rounds=rounds, provider_attempts_per_round=budget["maximum_provider_attempts"],
                maximum_provider_attempts_across_task=rounds * budget["maximum_provider_attempts"],
                maximum_tool_calls_across_task=rounds * budget["maximum_tool_calls"])


def configure(root, task, model, provider):
    settings = coding.configure(root, model, len(task["prompts"]))
    settings["rsi.agent"]["turn_budget"] = turn_budget(task)
    (root / "config/rsi/settings.json").write_text(json.dumps(settings))
    if provider:
        provider.configure(root)
    return settings


def object_at(value, *keys):
    for name in keys:
        if not isinstance(value, dict):
            return {}
        value = value.get(name)
    return value if isinstance(value, dict) else {}


def message_text(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    return "".join(part.get("text", "") for part in (content or []) if part.get("type") == "text")


def boundary_evidence(trace, command, stages):
    intents = [fact for fact in trace["facts"] if fact.get("type") == "tool_intent" and
               fact.get("name") == "bash" and object_at(fact, "arguments").get("command") == command]
    if len(intents) != stages:
        raise ValueError("missing actual Bash boundary probe")
    evidence = []
    for intent in intents:
        results = [fact for fact in trace["facts"] if fact.get("type") == "tool_result" and
                   fact.get("effect_id") == intent["effect_id"] and fact.get("turn_id") == intent["turn_id"] and fact["seq"] > intent["seq"]]
        if len(results) != 1:
            raise ValueError("boundary probe lacks its canonical result")
        result = results[0]["result"]
        value = object_at(result, "value")
        if (result.get("is_error") is not False or value.get("exit_code") != 0 or
                object_at(value, "stdout").get("text") != BOUNDARY_MARKER + "\n" or
                not any(stamp.get("filesystem") == "workspace_write" and stamp.get("scratch") == "private_tmp" and
                        object_at(stamp, "backend").get("kind") == "bubblewrap" for stamp in result.get("enforcement", []))):
            raise ValueError("actual Bash Tool did not enforce the frozen Host boundary")
        evidence.append({"intent_seq": intent["seq"], "result_seq": results[0]["seq"],
                         "effect_id": intent["effect_id"], "enforcement": result["enforcement"]})
    return evidence


def pressure_evidence(trace, requests, stages):
    """Single-flight scripted exchanges, durable output, and actual later wire input."""
    intents = [fact for fact in trace["facts"] if fact.get("type") == "model_intent"]
    if len(intents) != len(requests):
        raise ValueError("scripted exchanges do not match durable intent count")
    evidence = []
    for index, (intent, request) in enumerate(zip(intents, requests)):
        summary = object_at(intent, "purpose").get("kind") == "context_compaction"
        if summary != request["summary"]:
            raise ValueError("scripted exchange order does not match durable purpose")
        if not summary:
            continue
        plan = object_at(intent, "purpose", "plan")
        selected_answer = selected_pressure_answer(trace, plan, intent["seq"])
        events = [fact for fact in trace["facts"] if fact.get("type") == "model_event" and
                  fact.get("effect_id") == intent["effect_id"] and fact.get("turn_id") == intent["turn_id"] and
                  fact.get("purpose") == "context_compaction" and fact["seq"] > intent["seq"]]
        finished = [fact for fact in events if object_at(fact, "event").get("type") == "finished"]
        text = "".join(object_at(fact, "event", "delta").get("value", "") for fact in events
                       if object_at(fact, "event").get("type") == "content_delta" and
                       object_at(fact, "event", "delta").get("type") == "text")
        if (not plan.get("sources") or not plan.get("selections") or len(finished) != 1 or
                finished[0]["event"].get("reason") != "stop" or text != SUMMARY_TEXT):
            raise ValueError("summary lacks a selected durable source and successful exact output")
        if index + 1 >= len(intents):
            raise ValueError("summary has no later ordinary exchange")
        later, ordinary = intents[index + 1], requests[index + 1]
        if (ordinary["summary"] or ordinary["stage"] != request["stage"] or later["seq"] <= finished[0]["seq"] or
                later["turn_id"] != intent["turn_id"]):
            raise ValueError("summary is not followed by its ordinary continuation")
        framed = [message for message in ordinary["messages"] if message.get("role") in ("system", "developer") and
                  message_text(message) == SUMMARY_FRAME]
        source_text = "\n".join(map(message_text, request["messages"]))
        later_text = "\n".join(map(message_text, ordinary["messages"]))
        if len(framed) != 1 or PRESSURE_TEXT not in source_text or PRESSURE_TEXT in later_text:
            raise ValueError("ordinary input did not replace the selected answer with its framed summary")
        if len(later_text.encode()) >= len(source_text.encode()) // 2:
            raise ValueError("scripted compaction did not materially reduce provider input")
        evidence.append({"stage": request["stage"], "effect_id": intent["effect_id"],
                         "verified_selected_answer": selected_answer,
                         "intent_seq": intent["seq"], "finished_seq": finished[0]["seq"], "ordinary_seq": later["seq"],
                         "plan_view_sha256": plan["view_sha256"], "sources": plan["sources"],
                         "summary_sha256": hashlib.sha256(text.encode()).hexdigest(),
                         "ordinary_snapshot_request_sha256": later["snapshot"]["request_sha256"],
                         "ordinary_wire_messages_sha256": hashlib.sha256(json.dumps(ordinary["messages"], sort_keys=True).encode()).hexdigest()})
    if {item["stage"] for item in evidence} != {f"eval-goal-{stage + 1}" for stage in range(stages)}:
        raise ValueError("pressure evidence is missing for a task stage")
    return evidence


def fact_chain(facts):
    # session-protocol::advance_fact_prefix_digest, for the fixed JSON fixture.
    digest = bytes(32)
    for fact in facts:
        encoded = json.dumps(fact, ensure_ascii=False, separators=(",", ":")).encode()
        digest = hashlib.sha256(b"rsi-agent-context-fact-prefix-v2\0" + digest + encoded).digest()
    return digest.hex()


def selected_pressure_answer(trace, plan, intent_seq):
    """Verify this single-Session fixture's actual selected ordinary answer."""
    for source in plan.get("sources", []):
        if source.get("session") != "coding-session" or plan.get("session") != source["session"]:
            raise ValueError("pressure source is outside the fixture Session")
        facts = [fact for fact in trace["facts"] if fact.get("turn_id") == source.get("turn") and source["after_seq"] < fact["seq"] <= source["through_seq"]]
        if not facts or source["through_seq"] >= intent_seq or fact_chain(facts) != source["facts_sha256"]:
            raise ValueError("pressure source Fact chain differs from its frozen digest")
        message_index = 0
        text_by_effect = {}
        for fact in facts:
            kind = fact["type"]
            if kind in ("turn_accepted", "input_message_entered", "tool_result", "tool_rejected"):
                message_index += 1
            if kind != "model_event" or fact.get("purpose") != "conversation":
                continue
            event = object_at(fact, "event")
            delta = object_at(event, "delta")
            if event.get("type") == "content_delta" and delta.get("type") == "text":
                effect = fact["effect_id"]
                text_by_effect[effect] = text_by_effect.get(effect, "") + delta["value"]
            if event.get("type") == "finished":
                if text_by_effect.get(fact["effect_id"]) == PRESSURE_TEXT and event.get("reason") == "stop" and any(
                        selection.get("turn") == source["turn"] and selection["first"] <= message_index < selection["first"] + selection["count"]
                        for selection in plan.get("selections", [])):
                    return dict(turn=source["turn"], finished_seq=fact["seq"], message_index=message_index, facts_sha256=source["facts_sha256"])
                message_index += 1
    raise ValueError("plan does not select the actual pressure answer")


def classify(execution, reply, checked, request, expected_budget):
    if (execution.get("launch_error") or execution.get("output_limit_exceeded") or
            execution.get("cleanup_pending") or object_at(reply).get("cleanup_errors") or "infrastructure_error" in checked):
        return "infrastructure_failure"
    if execution["timed_out"] or checked.get("task_deadline_exceeded"):
        return "budget_exhausted"
    if not isinstance(reply, dict) or reply.get("transport") != "local_session_api" or any(object_at(reply, field).get("armed") is not False for field in ("before_live", "after_reads_live")):
        return "infrastructure_failure"
    goal = object_at(reply, "goal", "goal")
    if (goal.get("objective") != request["objective"] or goal.get("constraints") != request["constraints"] or
            json.dumps(goal.get("turn_budget"), sort_keys=True) != json.dumps(expected_budget, sort_keys=True)):
        return "infrastructure_failure"
    if (goal.get("id") != request["goal"] or object_at(reply, "receipt", "command").get("request_id") != request["request_id"] or
            type(goal.get("max_rounds")) is not int or goal["max_rounds"] != request["max_rounds"] or
            type(goal.get("allocated_rounds")) is not int or not 0 < goal["allocated_rounds"] <= goal["max_rounds"]):
        return "infrastructure_failure"
    reservation = object_at(goal, "reservation")
    settlement = object_at(reservation, "settlement")
    claim = object_at(goal, "report")
    if reservation.get("round") != goal["allocated_rounds"] or not settlement or object_at(reply, "live").get("armed") is not False:
        return "infrastructure_failure"
    if execution["exit_code"] != 0 or goal.get("phase") != "completed":
        exhausted = settlement.get("outcome") == "budget_exceeded" or (
            goal.get("phase") == "blocked" and settlement.get("outcome") == "completed" and
            not claim and goal["allocated_rounds"] == goal["max_rounds"])
        return "budget_exhausted" if exhausted else "agent_failure"
    if (claim.get("kind") != "complete" or not claim.get("evidence") or settlement.get("type") != "turn" or
            settlement.get("outcome") != "completed" or not claim.get("source_turn") or claim["source_turn"] != settlement.get("turn_id")):
        return "infrastructure_failure"
    return "passed" if checked["passed"] else "behavioral_failure"


def verify_counters(trace, budget, task_limits):
    turns = {}
    for fact in trace["facts"]:
        kind = fact["type"]
        if kind in ("model_started", "tool_started"):
            counts = turns.setdefault(fact["turn_id"], dict(model_started=0, tool_started=0))
            counts[kind] += 1
    for kind, field, aggregate in [("model_started", "maximum_provider_attempts", "maximum_provider_attempts_across_task"),
                                    ("tool_started", "maximum_tool_calls", "maximum_tool_calls_across_task")]:
        if any(counts[kind] > budget[field] for counts in turns.values()) or sum(counts[kind] for counts in turns.values()) > task_limits[aggregate]:
            raise ValueError(f"durable {kind} counters exceed the frozen allowance")
    if len(turns) > task_limits["maximum_parent_rounds"]:
        raise ValueError("durable execution exceeds the parent round cap")
    return turns


def grade_before_deadline(workspace, task, stage, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        return {"passed": False, "task_deadline_exceeded": True}
    checked = coding.oracle(workspace, task, stage, timeout_seconds=min(30, remaining))
    if time.monotonic() >= deadline:
        checked["task_deadline_exceeded"] = True
    return checked


def run_task(task, binary, destination, model, key, provider):
    destination.mkdir()
    started = time.monotonic()
    report = {"task": task["id"], "task_version": coding.TASK_VERSION, "harness": "session-api-goal-1",
              "evidence_kind": "scripted_api_mechanism" if provider else "live_session_api",
              "scripted_pressure": bool(provider and provider.pressure),
              "requested_model": model, "limits": limits(task, turn_budget(task)), "stages": [],
              "provider_attempts": 0, "tool_calls": 0, "compaction_intents": 0, "compaction_finished_events": 0}
    try:
        report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        execute_task(task, binary, destination, model, key, provider, report, started)
    except Exception as error:
        report["classification"] = "infrastructure_failure"
        report["evidence_error"] = redacted(f"{type(error).__name__}: {error}", key)[:4096]
    report["elapsed_seconds"] = round(time.monotonic() - started, 3)
    safe = redacted(report, key)
    if safe != report:
        safe["classification"] = "infrastructure_failure"
        safe["credential_redacted"] = True
    persist(destination / "report.json", safe, key)
    return safe


def execute_task(task, binary, destination, model, key, provider, report, started):
    request_start = len(provider.requests) if provider else 0
    persist(destination / "admission.json", report, key)
    # The local API appends a 64-character state identity and socket filename.
    # Keep this isolated runtime path within Linux sockaddr_un's 107 bytes.
    with tempfile.TemporaryDirectory(prefix="rsi-api-", dir="/tmp") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        coding.write_project(workspace, task["initial"])
        report["settings"] = configure(root, task, model, provider)
        frozen_config = {path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                         for path in (root / "config").rglob("*") if path.is_file()}
        report["config_sha256_before"] = frozen_config
        env = coding.clean_environment()
        env.update({"XDG_CONFIG_HOME": str(root / "config"), "XDG_STATE_HOME": str(root / "state"),
                    "XDG_CACHE_HOME": str(root / "cache"), "XDG_RUNTIME_DIR": str(root / "r")})
        env["RSI_OPENAI_COMPATIBLE_API_KEY" if provider else "DEEPSEEK_API_KEY"] = key
        (root / "r").mkdir(mode=0o700)
        initial = grade_before_deadline(workspace, task, 0, started + TASK_SECONDS)
        report["initial_oracle"] = initial
        if initial["passed"] or "infrastructure_error" in initial or "admission_error" in initial:
            report["classification"] = "infrastructure_failure"
        elif initial.get("task_deadline_exceeded"):
            report["classification"] = "budget_exhausted"
        for stage, prompt in enumerate(task["prompts"]):
            if report.get("classification"):
                break
            if provider:
                provider.stage(task, stage)
            request = {"session": "coding-session", "goal": f"eval-goal-{stage + 1}", "request_id": f"eval-create-{stage + 1}",
                       "create_session": stage == 0, "objective": prompt,
                       "constraints": "Use the fixed existing source layout. Report Goal completion with report_goal only after meaningful checks. An external oracle independently grades the source.",
                       "max_rounds": ROUNDS}
            remaining = TASK_SECONDS - (time.monotonic() - started)
            try:
                command = coding.agent_command(binary, root, env, [])
                execution = coding.run_oracle_process(command, env, remaining, input_bytes=json.dumps(request).encode(),
                                                     maximum_output_bytes=coding.MAXIMUM_AGENT_OUTPUT_BYTES)
            except OSError as error:
                execution = {"exit_code": None, "timed_out": False, "stdout": "", "stderr": "", "launch_error": str(error)}
            persist(destination / f"stage-{stage + 1}.stdout", execution["stdout"], key)
            persist(destination / f"stage-{stage + 1}.stderr", execution["stderr"], key)
            try:
                reply = json.loads(execution["stdout"])
            except (ValueError, TypeError):
                reply = None
            checked = grade_before_deadline(workspace, task, stage, started + TASK_SECONDS)
            outcome = classify(execution, reply, checked, request, turn_budget(task))
            if provider and outcome == "passed" and reply["goal"]["goal"]["allocated_rounds"] != ROUNDS:
                outcome = "infrastructure_failure"
            report["stages"].append({"request": request, "api": reply, "oracle": checked, "classification": outcome,
                                     **{name: value for name, value in execution.items() if name not in ("stdout", "stderr")}})
            if outcome != "passed":
                report["classification"] = outcome
        trace = coding.durable_trace(root)
        coding.record_trace_summary(report, trace)
        try:
            report["verified_turn_counters"] = verify_counters(trace, turn_budget(task), report["limits"])
        except (KeyError, TypeError, ValueError) as error:
            report["classification"] = "infrastructure_failure"
            report["counter_error"] = str(error)
        report["config_sha256_after"] = {path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                                          for path in (root / "config").rglob("*") if path.is_file()}
        if report["config_sha256_after"] != frozen_config:
            report["classification"] = "infrastructure_failure"
            report["config_drift"] = True
        if provider:
            try:
                report["tool_boundary"] = boundary_evidence(trace, provider.boundary_command, len(task["prompts"]))
            except (KeyError, TypeError, ValueError) as error:
                report["boundary_evidence_error"] = str(error)
                report["classification"] = "infrastructure_failure"
        if provider and provider.pressure:
            try:
                report["compaction_reuse"] = pressure_evidence(trace, provider.requests[request_start:], len(task["prompts"]))
            except (KeyError, TypeError, ValueError) as error:
                report["compaction_evidence_error"] = str(error)
                report["classification"] = "infrastructure_failure"
        report["compaction_intents"] = sum(fact.get("type") == "model_intent" and fact.get("purpose", {}).get("kind") == "context_compaction" for fact in trace["facts"])
        # Finished output is an installation candidate; policy/source validation
        # decides usability. Do not label event counting as installed summaries.
        report["compaction_finished_events"] = sum(fact.get("type") == "model_event" and fact.get("purpose") == "context_compaction" and fact.get("event", {}).get("type") == "finished" for fact in trace["facts"])
        persist(destination / "durable-trace.json", trace, key)
        try:
            sources = coding.safe_sources(workspace, task)
            persist(destination / "source-evidence.json", sources, key)
            diff = "".join("".join(difflib.unified_diff(task["initial"][name].splitlines(True), text.splitlines(True), fromfile="a/" + name, tofile="b/" + name)) for name, text in sources.items())
            persist(destination / "source.diff", diff, key)
        except (ValueError, OSError) as error:
            report["source_admission_error"] = str(error)
            if report["classification"] == "passed":
                report["classification"] = "behavioral_failure"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true")
    mode.add_argument("--live", action="store_true")
    parser.add_argument("--key-file", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=coding.ROOT / "target/debug/examples/session-api-eval")
    parser.add_argument("--model", default="deepseek-flash")
    parser.add_argument("--task", choices=[task["id"] for task in coding.TASKS], action="append")
    parser.add_argument("--pressure", action="store_true")
    args = parser.parse_args()
    if args.pressure and not args.self_test:
        parser.error("scripted context pressure is mechanism evidence only")
    if args.live and (args.key_file is None or not re.fullmatch(r"[A-Za-z0-9_.-]{1,256}", args.model)):
        parser.error("live mode needs an authorized key file and exact model identifier")
    if not args.binary.is_file():
        parser.error("build cargo build -p rsi --example session-api-eval first")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / "session-api-eval"
    shutil.copy2(args.binary.resolve(), frozen)
    with frozen.open("rb") as executable:
        (output / "binary.json").write_text(json.dumps({"sha256": hashlib.file_digest(executable, "sha256").hexdigest()}) + "\n")
    key = coding.read_key(args.key_file) if args.live else "session-api-fixture-secret"
    provider = Provider(args.pressure) if args.self_test else None
    reports = []
    try:
        if args.self_test:
            coding.self_test()
        selected = [task for task in coding.TASKS if not args.task or task["id"] in args.task]
        for task in selected:
            report = run_task(task, frozen, output / task["id"], "fixture-model" if provider else args.model, key, provider)
            reports.append(report)
            persist(output / "results.json", reports, key)
            print(json.dumps({name: report[name] for name in ["task", "classification", "elapsed_seconds", "provider_attempts", "tool_calls", "compaction_intents"]}), flush=True)
        if provider:
            persist(output / "scripted-provider.json", provider.requests, key)
    finally:
        if provider:
            provider.close()
        persist(output / "results.json", reports, key)
    return 0 if len(reports) == len(selected) and all(report["classification"] == "passed" for report in reports) else 1


if __name__ == "__main__":
    raise SystemExit(main())
