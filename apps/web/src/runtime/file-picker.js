const pathHex = (text) =>
  [...new TextEncoder().encode(text)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");

// Rust owns the read and locator; this owner holds only its bounded page and insertion range.
export function createFilePicker({ request, active, insert, changed, closed }) {
  let serial = 0,
    retired = false;
  const alive = () => !retired && active();
  const picker = {
    page: undefined,
    loading: false,
    status: "",
    filter: "",
    async read(operation) {
      if (!alive() || picker.loading) return;
      const version = ++serial;
      picker.loading = true;
      picker.status = "Reading workspace…";
      changed();
      try {
        const page = await request(operation);
        if (!alive() || serial !== version) return;
        const bytes = Uint8Array.from(page.path.match(/../g) ?? [], (pair) =>
          parseInt(pair, 16),
        );
        let path;
        try {
          path = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
        } catch {
          path = `path_hex:${page.path}`;
        }
        picker.page = {
          ...page,
          label: path || "Workspace root",
          parent: page.path
            ? page.path.slice(0, Math.max(0, bytes.lastIndexOf(47)) * 2)
            : undefined,
        };
        picker.filter = "";
        picker.status = "";
      } catch (error) {
        if (alive() && serial === version)
          picker.status = `Files unavailable: ${error.message}. Reopen a path to refresh; the draft is unchanged.`;
      } finally {
        if (alive() && serial === version) {
          picker.loading = false;
          changed();
        }
      }
    },
    open(path, file_kind) {
      return picker.read({ kind: "open", path, file_kind });
    },
    openText(path, kind) {
      if (new TextEncoder().encode(path).length > 4096) {
        picker.status = "Path exceeds 4 KiB";
        changed();
        return;
      }
      return picker.open(pathHex(path === "." ? "" : path), kind);
    },
    setFilter(value) {
      picker.filter = value;
      changed();
    },
    insert() {
      if (!alive() || picker.loading || picker.page?.kind !== "file") return;
      try {
        insert(picker.page.locator);
        picker.close();
      } catch (error) {
        picker.status = `Path was not inserted: ${error.message}`;
        changed();
      }
    },
    close() {
      if (retired) return;
      retired = true;
      serial++;
      const operation =
        picker.page && !picker.loading
          ? { kind: "release", revision: picker.page.revision }
          : null;
      void request(operation).catch(() => {});
      closed();
    },
  };
  return picker;
}
