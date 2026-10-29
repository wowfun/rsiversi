// Own the temporary capture until it is either added to the draft or released.
export function createReferencePicker({
  existing,
  request,
  release,
  add,
  canAdd,
  active,
  describe,
  changed,
  closed,
}) {
  let revision = 0,
    retired = false,
    pendingCapture,
    releasing;
  const alive = () => !retired && active();
  const releaseCapture = async () => {
    const reference = pendingCapture;
    if (!reference) return;
    if (!releasing)
      releasing = Promise.resolve(release(reference))
        .then(() => {
          if (pendingCapture === reference) pendingCapture = undefined;
        })
        .finally(() => { releasing = undefined; });
    await releasing;
  };
  const show = (reference, page) => {
    const next =
      page?.next_offset ?? new TextEncoder().encode(reference.preview).length;
    picker.preview = {
      ...describe(reference.metadata),
      text: page?.text ?? reference.preview,
      previous: page?.offset > 0 ? Math.max(0, page.offset - 8192) : undefined,
      next:
        (page?.has_more ?? next < reference.metadata.text_bytes)
          ? next
          : undefined,
      reference,
    };
  };
  const picker = {
    existing: !!existing,
    capturing: false,
    status: "",
    preview: undefined,
    get addDisabled() {
      return !picker.preview || !canAdd(picker.preview.reference);
    },
    async capture(source) {
      if (!alive() || picker.capturing) return;
      const version = ++revision;
      picker.capturing = true;
      picker.preview = undefined;
      picker.status = "Capturing conversation…";
      changed();
      try {
        await releaseCapture();
        if (!alive() || version !== revision) return;
        const reference = await request({
          kind: "capture",
          source: source.trim(),
        });
        pendingCapture = reference;
        if (!alive() || version !== revision) {
          await releaseCapture();
          return;
        }
        picker.status = "Captured. Review it before adding to the draft.";
        show(reference);
      } catch (error) {
        if (alive() && version === revision)
          picker.status = `${error.message.startsWith("Reference cleanup failed:") ? error.message : `Capture failed: ${error.message}`}. Your draft is unchanged.`;
      } finally {
        picker.capturing = false;
        if (alive()) changed();
      }
    },
    async page(offset) {
      if (!alive() || !picker.preview) return;
      const reference = picker.preview.reference,
        version = ++revision;
      picker.status = "Reading frozen reference…";
      changed();
      try {
        const page = await request({
          kind: "preview",
          reference,
          offset,
          maximum: 8192,
        });
        if (!alive() || version !== revision) return;
        picker.status = "";
        show(reference, page);
      } catch (error) {
        if (alive() && version === revision)
          picker.status = `Reference unavailable: ${error.message}. Your draft is unchanged.`;
      } finally {
        if (alive()) changed();
      }
    },
    add() {
      if (!alive() || existing || picker.addDisabled) return;
      add(picker.preview.reference);
      pendingCapture = undefined;
      picker.close();
    },
    close() {
      if (retired) return;
      retired = true;
      revision++;
      // release() retains a retryable cleanup on failure; closing never discards it.
      void releaseCapture().catch(() => {});
      closed();
    },
  };
  if (existing) show(existing);
  return picker;
}
