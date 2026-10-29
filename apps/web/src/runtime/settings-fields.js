import {
  formUnavailable,
  settingsLimitExceeded,
  SETTINGS_LIMIT_MESSAGE,
} from "../../settings-json.js";
const own = (value, key) => Object.prototype.hasOwnProperty.call(value, key);
const simple = new Set([
  "type",
  "title",
  "description",
  "default",
  "properties",
  "required",
  "additionalProperties",
  "enum",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "multipleOf",
  "minLength",
  "maxLength",
  "pattern",
  "$schema",
]);
const empty = (schema) =>
  !schema || typeof schema !== "object"
    ? null
    : own(schema, "default")
      ? structuredClone(schema.default)
      : ({ object: {}, string: "", boolean: false, number: 0, integer: 0 }[
          schema.type
        ] ?? null);

export function settingsFields(schema, value) {
  let count = 0;
  function field(schema, value, label, depth) {
    const valid =
        schema && typeof schema === "object" && !Array.isArray(schema),
      supported =
        valid &&
        Object.keys(schema).every((key) => simple.has(key)) &&
        ++count <= 64 &&
        depth <= 8;
    const title =
      valid && typeof schema.title === "string"
        ? schema.title
        : label
            .split(" / ")
            .at(-1)
            .replaceAll("_", " ")
            .replace(/^./, (ch) => ch.toUpperCase());
    const item = {
      label,
      title,
      schema: valid ? schema : {},
      description:
        valid && typeof schema.description === "string"
          ? schema.description
          : "",
    };
    if (
      supported &&
      Array.isArray(schema.enum) &&
      schema.enum.length > 0 &&
      schema.enum.length <= 64 &&
      schema.enum.every(
        (item) =>
          item === null ||
          ["string", "boolean", "number"].includes(typeof item),
      ) &&
      schema.enum.some((item) => Object.is(item, value))
    )
      return {
        ...item,
        kind: "enum",
        value: String(schema.enum.findIndex((item) => Object.is(item, value))),
      };
    if (
      supported &&
      schema.type === "object" &&
      value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      schema.properties &&
      typeof schema.properties === "object" &&
      !Array.isArray(schema.properties) &&
      Object.keys(schema.properties).length <= 32 &&
      Object.keys(value).every((key) => own(schema.properties, key))
    )
      return {
        ...item,
        kind: "object",
        parts: Object.entries(schema.properties).map(([key, child]) => ({
          key,
          included: own(value, key),
          required:
            Array.isArray(schema.required) && schema.required.includes(key),
          field: field(
            child,
            own(value, key) ? value[key] : empty(child),
            `${label} / ${key}`,
            depth + 1,
          ),
        })),
      };
    if (
      supported &&
      ["string", "boolean", "number", "integer"].includes(schema.type) &&
      typeof value === (schema.type === "integer" ? "number" : schema.type) &&
      !schema.enum &&
      !(schema.type === "string" && /\r/.test(value))
    )
      return {
        ...item,
        kind: schema.type,
        value: schema.type === "boolean" ? value : String(value),
      };
    return { ...item, kind: "json", value: JSON.stringify(value, null, 2) };
  }
  return field(schema, value, "Settings", 0);
}
export function fieldsJson(field) {
  if (field.kind === "object")
    return `{${field.parts
      .filter((part) => part.required || part.included)
      .map((part) => `${JSON.stringify(part.key)}:${fieldsJson(part.field)}`)
      .join(",")}}`;
  if (field.kind === "enum")
    return JSON.stringify(field.schema.enum[Number(field.value)]);
  if (["json", "number", "integer"].includes(field.kind)) {
    JSON.parse(field.value);
    return field.value;
  }
  return JSON.stringify(field.value);
}
export function createSettingsFields(schema, original, disabled, changed) {
  const initialOversized = settingsLimitExceeded(original);
  let edited = false;
  const state = {
    schema,
    text: original,
    jsonMode: !!formUnavailable(original),
    locked: disabled || initialOversized,
    initialOversized,
    reason: "",
    fields: undefined,
    get toggleDisabled() {
      return initialOversized || (state.jsonMode && settingsLimitExceeded(state.text));
    },
    edit(field, value) {
      if (state.locked) return;
      field.value = value;
      edited = true;
      state.reason = "";
      changed(true);
    },
    include(part, value) {
      if (state.locked) return;
      part.included = value;
      edited = true;
      state.reason = "";
      changed(true);
    },
    editJson(text) {
      if (state.locked) return;
      state.text = text;
      state.reason = settingsLimitExceeded(text)
        ? SETTINGS_LIMIT_MESSAGE
        : "";
      changed(true);
    },
    toggle() {
      if (initialOversized) return;
      try {
        if (state.jsonMode) {
          const why = formUnavailable(state.text);
          if (why) {
            state.reason = why;
            changed(false);
            return;
          }
          state.fields = settingsFields(schema, JSON.parse(state.text));
          edited = false;
          state.jsonMode = false;
        } else {
          state.text = state.value();
          state.jsonMode = true;
        }
        state.reason = "";
      } catch (error) {
        state.reason = String(error.message ?? error);
      }
      changed(false);
    },
    value() {
      return state.jsonMode || !edited ? state.text : fieldsJson(state.fields);
    },
    json() {
      const text = state.value();
      if (settingsLimitExceeded(text))
        throw Error(SETTINGS_LIMIT_MESSAGE);
      JSON.parse(text);
      return text;
    },
    canSave() {
      if (state.locked) return false;
      try {
        state.json();
        return true;
      } catch {
        return false;
      }
    },
  };
  if (!state.jsonMode)
    state.fields = settingsFields(schema, JSON.parse(original));
  return state;
}
