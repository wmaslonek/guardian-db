import type { Document, DocumentId } from "./types.js";
import { ValidationError } from "./errors.js";

export function clone<T>(value: T): T {
  if (typeof globalThis.structuredClone === "function") {
    return globalThis.structuredClone(value);
  }
  return JSON.parse(JSON.stringify(value)) as T;
}

export function assertDocument(value: unknown): asserts value is Document {
  if (value === null || Array.isArray(value) || typeof value !== "object") {
    throw new ValidationError("$document", "document must be a plain object");
  }
}

export function getPath(document: unknown, path: string): unknown {
  let current: unknown = document;
  for (const segment of path.split(".")) {
    if (current === null || typeof current !== "object" || Array.isArray(current)) {
      return undefined;
    }
    current = (current as Document)[segment];
  }
  return current;
}

export function setPath(document: Document, path: string, value: unknown): boolean {
  const segments = path.split(".");
  if (segments.length === 0 || segments.some((segment) => segment.length === 0)) {
    throw new ValidationError(path, "invalid field path");
  }

  let current = document;
  for (const segment of segments.slice(0, -1)) {
    const next = current[segment];
    if (next === undefined) {
      current[segment] = {};
    } else if (next === null || Array.isArray(next) || typeof next !== "object") {
      throw new ValidationError(path, "cannot traverse a non-object value");
    }
    current = current[segment] as Document;
  }

  const key = segments.at(-1)!;
  const changed = !deepEqual(current[key], value);
  current[key] = clone(value);
  return changed;
}

export function unsetPath(document: Document, path: string): boolean {
  const segments = path.split(".");
  if (segments.length === 0 || segments.some((segment) => segment.length === 0)) {
    throw new ValidationError(path, "invalid field path");
  }

  let current: Document = document;
  for (const segment of segments.slice(0, -1)) {
    const next = current[segment];
    if (next === null || Array.isArray(next) || typeof next !== "object") {
      return false;
    }
    current = next as Document;
  }
  return delete current[segments.at(-1)!];
}

export function deepEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) {
    return true;
  }
  if (typeof left !== typeof right || left === null || right === null) {
    return false;
  }
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((value, index) => deepEqual(value, right[index]));
  }
  if (typeof left === "object" && !Array.isArray(left) && !Array.isArray(right)) {
    const leftRecord = left as Document;
    const rightRecord = right as Document;
    const leftKeys = Object.keys(leftRecord);
    const rightKeys = Object.keys(rightRecord);
    return (
      leftKeys.length === rightKeys.length &&
      leftKeys.every((key) => Object.hasOwn(rightRecord, key) && deepEqual(leftRecord[key], rightRecord[key]))
    );
  }
  return false;
}

export function canonicalId(value: unknown): string {
  if (typeof value !== "string" && typeof value !== "number" && typeof value !== "boolean") {
    throw new ValidationError("$id", "primary keys must be strings, numbers, or booleans");
  }
  return String(value);
}

export function asDocumentId(value: unknown): DocumentId {
  canonicalId(value);
  return value as DocumentId;
}

export function randomId(): string {
  const cryptoObject = globalThis.crypto;
  if (cryptoObject?.randomUUID !== undefined) {
    return cryptoObject.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

export function indexToken(value: unknown): string {
  const serialized = JSON.stringify(canonicalIndexValue(value));
  return serialized === undefined ? String(value) : serialized;
}

function canonicalIndexValue(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(canonicalIndexValue);
  }
  if (value !== null && typeof value === "object") {
    const prototype = Object.getPrototypeOf(value);
    if (prototype === Object.prototype || prototype === null) {
      return Object.fromEntries(
        Object.entries(value as Document)
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([key, nested]) => [key, canonicalIndexValue(nested)]),
      );
    }
  }
  return value;
}
