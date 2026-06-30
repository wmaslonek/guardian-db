import { ValidationError } from "./errors.js";
import { assertDocument, clone, randomId } from "./utils.js";
import type {
  Document,
  FieldSchema,
  NormalizedFieldSchema,
  NormalizedSchema,
  ScalarFieldType,
  SchemaDefinition,
} from "./types.js";

export function defineSchema<T extends Document>(definition: SchemaDefinition<T>): SchemaDefinition<T> {
  return definition;
}

export function normalizeSchema<T extends Document>(definition?: SchemaDefinition<T>): NormalizedSchema {
  const fields: Record<string, NormalizedFieldSchema> = {};
  let primaryKey: string | undefined;

  for (const [name, field] of Object.entries(definition?.fields ?? {})) {
    const normalized = normalizeField(field);
    if (normalized.primaryKey) {
      if (primaryKey !== undefined) {
        throw new ValidationError(name, "only one primary key is supported");
      }
      primaryKey = name;
      normalized.required = true;
      normalized.unique = true;
      normalized.index = true;
    }
    if (normalized.unique) {
      normalized.index = true;
    }
    fields[name] = normalized;
  }

  primaryKey ??= Object.hasOwn(fields, "_id") ? "_id" : Object.hasOwn(fields, "id") ? "id" : "_id";
  if (!Object.hasOwn(fields, primaryKey)) {
    fields[primaryKey] = {
      type: "string",
      required: true,
      nullable: false,
      primaryKey: true,
      unique: true,
      index: true,
    };
  } else {
    fields[primaryKey]!.primaryKey = true;
    fields[primaryKey]!.required = true;
    fields[primaryKey]!.unique = true;
    fields[primaryKey]!.index = true;
  }

  const timestamps = normalizeTimestamps(definition?.timestamps);
  if (timestamps !== undefined) {
    fields[timestamps.createdAt] ??= normalizeField({ type: "timestamp" });
    fields[timestamps.updatedAt] ??= normalizeField({ type: "timestamp" });
  }

  return {
    fields,
    primaryKey,
    strict: definition?.strict ?? definition !== undefined,
    ...(timestamps === undefined ? {} : { timestamps }),
    version: definition?.version ?? 1,
  };
}

export function prepareInsert(schema: NormalizedSchema, input: Document): Document {
  const document = clone(input);
  assertDocument(document);

  for (const [field, definition] of Object.entries(schema.fields)) {
    if (document[field] === undefined && definition.default !== undefined) {
      document[field] = clone(
        typeof definition.default === "function"
          ? (definition.default as () => unknown)()
          : definition.default,
      );
    }
  }

  if (document[schema.primaryKey] === undefined || document[schema.primaryKey] === null) {
    document[schema.primaryKey] = randomId();
  }

  if (schema.timestamps !== undefined) {
    const now = new Date().toISOString();
    document[schema.timestamps.createdAt] ??= now;
    document[schema.timestamps.updatedAt] = now;
  }

  validateDocument(schema, document);
  return document;
}

export function touchUpdatedAt(schema: NormalizedSchema, document: Document): void {
  if (schema.timestamps !== undefined) {
    document[schema.timestamps.updatedAt] = new Date().toISOString();
  }
}

export function validateDocument(schema: NormalizedSchema, document: Document): void {
  assertDocument(document);

  for (const [field, definition] of Object.entries(schema.fields)) {
    const value = document[field];
    if (value === undefined) {
      if (definition.required) {
        throw new ValidationError(field, "required field is missing");
      }
      continue;
    }
    if (value === null) {
      if (definition.required && !definition.nullable) {
        throw new ValidationError(field, "required field cannot be null");
      }
      continue;
    }
    if (!matchesType(definition.type, value)) {
      throw new ValidationError(field, `expected ${definition.type}`);
    }
    const customResult = definition.validate?.(value, document);
    if (customResult === false) {
      throw new ValidationError(field, "custom validator rejected the value");
    }
    if (typeof customResult === "string") {
      throw new ValidationError(field, customResult);
    }
  }

  if (schema.strict) {
    for (const field of Object.keys(document)) {
      if (!Object.hasOwn(schema.fields, field)) {
        throw new ValidationError(field, "field is not declared in the strict schema");
      }
    }
  }
}

function normalizeField(field: FieldSchema = {}): NormalizedFieldSchema {
  return {
    type: normalizeType(field.type),
    required: field.required ?? false,
    nullable: field.nullable ?? false,
    primaryKey: field.primaryKey ?? false,
    unique: field.unique ?? false,
    index: field.index ?? false,
    ...(field.default === undefined ? {} : { default: field.default }),
    ...(field.validate === undefined
      ? {}
      : { validate: field.validate as (value: unknown, document: Readonly<Document>) => boolean | string }),
  };
}

function normalizeType(type: FieldSchema["type"]): ScalarFieldType {
  if (type === undefined) return "any";
  if (type === String) return "string";
  if (type === Number) return "number";
  if (type === Boolean) return "boolean";
  if (type === Object) return "object";
  if (type === Array) return "array";
  if (typeof type === "string") return type;
  return "any";
}

function normalizeTimestamps(
  timestamps: SchemaDefinition["timestamps"],
): { createdAt: string; updatedAt: string } | undefined {
  if (timestamps === undefined || timestamps === false) {
    return undefined;
  }
  if (timestamps === true) {
    return { createdAt: "createdAt", updatedAt: "updatedAt" };
  }
  return {
    createdAt: timestamps.createdAt ?? "createdAt",
    updatedAt: timestamps.updatedAt ?? "updatedAt",
  };
}

function matchesType(type: ScalarFieldType, value: unknown): boolean {
  switch (type) {
    case "any":
      return true;
    case "string":
    case "timestamp":
      return typeof value === "string";
    case "number":
      return typeof value === "number" && Number.isFinite(value);
    case "boolean":
      return typeof value === "boolean";
    case "object":
      return value !== null && typeof value === "object" && !Array.isArray(value);
    case "array":
      return Array.isArray(value);
  }
}
