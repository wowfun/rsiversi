// Candidates are opened through Rust before a selected UTF-8 range is frozen.
export function createHistoryPicker({
  context,
  workspaces,
  sources,
  request,
  release,
  add,
  active,
  changed,
  closed,
}) {
  let retired = false,
    serial = 0,
    continuation,
    pendingReference;
  const alive = () => !retired && active();
  const cleanup = (reference) => {
    if (reference) void release(reference).catch(() => {});
  };
  const replaceBody = (body) => {
    cleanup(pendingReference);
    pendingReference = undefined;
    picker.body = body;
  };
  const coverage = (value) =>
    `Indexed through ${value.indexed_through}; observed through ${value.observed_through}; omitted ${value.omissions}. ${value.has_more ? "More indexing needed." : "Caught up at this observation."}`;
  const progress = (value) => {
    continuation = value.continuation;
    return `Authorized sources ${value.visible_sources}; pending ${value.pending_sources}. ${value.discovery_complete ? "Discovery reached the end of this pass." : "Discovery is partial."}${value.metadata_unavailable ? " Saved metadata is unavailable; refresh discovery to retry." : ""}${value.capacity_limited ? " Capacity reached; some saved history is omitted." : ""}`;
  };
  const perform = async (operation, render) => {
    if (!alive() || picker.busy) return;
    picker.busy = true;
    const current = ++serial;
    picker.status = "Reading history owner…";
    changed();
    try {
      const reply = await request(operation);
      if (alive() && serial === current) render(reply);
      else if (operation.operation === "freeze") cleanup(reply.reference);
    } catch (error) {
      if (alive() && serial === current) picker.status = error.message;
    } finally {
      picker.busy = false;
      if (alive()) changed();
    }
  };
  const picker = {
    busy: false,
    status: "",
    body: undefined,
    workspaces,
    sources,
    fields: {
      range: "accessible_host",
      workspace: context.workspace,
      kind: "native",
      source: "",
      query: "",
    },
    set(field, value) {
      if (!alive() || picker.busy) return;
      picker.fields[field] = value;
      serial++;
      if (field !== "query") continuation = undefined;
      replaceBody(undefined);
      picker.status =
        "Search conditions changed. Search again to show current matches.";
      changed();
    },
    choose(index) {
      const source = sources[index];
      if (!source || picker.busy) return;
      picker.set("source", source.id);
      picker.set("kind", source.kind);
    },
    scope() {
      const f = picker.fields;
      return f.range === "accessible_host"
        ? { kind: "accessible_host" }
        : f.range === "workspace"
          ? { kind: "workspace", workspace: f.workspace }
          : {
              kind: "conversation",
              source: {
                workspace: f.workspace,
                conversation: { kind: f.kind, id: f.source.trim() },
              },
            };
    },
    discover(reset = false) {
      return perform(
        {
          operation: "discover",
          scope: picker.scope(),
          after: reset ? null : (continuation ?? null),
        },
        (reply) => {
          picker.status = progress(reply.progress);
          replaceBody(undefined);
        },
      );
    },
    search(after = null, scope = picker.scope(), query = picker.fields.query) {
      return perform({ operation: "query", scope, query, after }, (reply) => {
        replaceBody(undefined);
        if (reply.kind === "stale") {
          picker.status = reply.reason;
          return;
        }
        picker.status = progress(reply.progress);
        picker.body = {
          kind: "matches",
          matches: reply.matches,
          next: reply.next,
          scope,
          query,
        };
      });
    },
    read(scope, hit, offset = 0, referenceAllowed = true) {
      return perform({ operation: "read", scope, hit, offset }, (reply) => {
        picker.status = `Verified original · record ${hit.original.record.sequence} · bytes ${reply.offset}–${reply.next_offset} of ${hit.original.end}`;
        replaceBody({
          kind: "original",
          scope,
          hit,
          referenceAllowed,
          ...reply,
        });
      });
    },
    freeze(selectionStart, selectionEnd) {
      const body = picker.body;
      if (body?.kind !== "original" || !body.referenceAllowed) return;
      const start =
          body.offset +
          new TextEncoder().encode(body.text.slice(0, selectionStart)).length,
        end =
          body.offset +
          new TextEncoder().encode(body.text.slice(0, selectionEnd)).length;
      if (start === end) {
        picker.status = "Select a nonempty fragment first.";
        changed();
        return;
      }
      return perform(
        {
          operation: "freeze",
          scope: body.scope,
          hit: body.hit,
          target: context.session,
          start,
          end,
        },
        (reply) => {
          picker.status =
            "Frozen. Review these exact bytes before adding to your draft.";
          replaceBody({ kind: "frozen", reference: reply.reference });
          pendingReference = reply.reference;
        },
      );
    },
    add() {
      if (!alive() || picker.busy || !pendingReference) return;
      add(pendingReference);
      pendingReference = undefined;
      picker.close();
    },
    inspect(after = null, scope = picker.scope()) {
      return perform({ operation: "progress", scope, after }, (reply) => {
        picker.status = progress(reply.progress);
        replaceBody({
          kind: "coverage",
          sources: reply.sources,
          next: reply.next,
          scope,
        });
      });
    },
    rebuild(scope) {
      return perform({ operation: "rebuild", scope }, (reply) => {
        picker.status = coverage(reply.coverage);
      });
    },
    reset(after = null) {
      return perform(
        {
          operation: "reset",
          scope: { kind: "workspace", workspace: picker.fields.workspace },
          after,
        },
        (reply) => {
          picker.status = "Workspace index reset · " + progress(reply.progress);
          replaceBody({ kind: "reset", next: reply.next });
        },
      );
    },
    coverage,
    close() {
      if (retired) return;
      retired = true;
      serial++;
      replaceBody(undefined);
      closed();
    },
  };
  return picker;
}
