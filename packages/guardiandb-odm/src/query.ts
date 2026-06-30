import { GuardianDBError } from "./errors.js";
import { deepEqual, getPath, setPath, unsetPath } from "./utils.js";
import type { Document, Query, UpdateOperations } from "./types.js";

export function matchesQuery<T extends Document>(document: T, query: Query<T>): boolean {
  for (const [field, condition] of Object.entries(query)) {
    if (field === "$and" || field === "$or" || field === "$nor") {
      if (!Array.isArray(condition)) {
        throw new GuardianDBError("INVALID_QUERY", `${field} expects an array`);
      }
      const clauses = condition as Query<T>[];
      if (field === "$and" && !clauses.every((clause) => matchesQuery(document, clause))) return false;
      if (field === "$or" && !clauses.some((clause) => matchesQuery(document, clause))) return false;
      if (field === "$nor" && clauses.some((clause) => matchesQuery(document, clause))) return false;
      continue;
    }
    if (field.startsWith("$")) {
      throw new GuardianDBError("INVALID_QUERY", `Unsupported logical operator '${field}'`);
    }
    if (!matchesCondition(getPath(document, field), condition)) {
      return false;
    }
  }
  return true;
}

export function applyUpdate<T extends Document>(
  document: T,
  operations: UpdateOperations<T>,
  immutableFields: ReadonlySet<string>,
): boolean {
  const keys = Object.keys(operations);
  if (keys.some((key) => !key.startsWith("$"))) {
    throw new GuardianDBError(
      "INVALID_UPDATE",
      "Replacement updates are not supported; use operators such as $set",
    );
  }

  let changed = false;
  if (operations.$set !== undefined) {
    for (const [path, value] of Object.entries(operations.$set)) {
      assertMutable(path, immutableFields);
      changed = setPath(document, path, value) || changed;
    }
  }
  if (operations.$unset !== undefined) {
    for (const path of Object.keys(operations.$unset)) {
      assertMutable(path, immutableFields);
      changed = unsetPath(document, path) || changed;
    }
  }
  if (operations.$inc !== undefined) {
    for (const [path, increment] of Object.entries(operations.$inc)) {
      assertMutable(path, immutableFields);
      if (typeof increment !== "number" || !Number.isFinite(increment)) {
        throw new GuardianDBError("INVALID_UPDATE", `$inc value for '${path}' must be numeric`);
      }
      const current = getPath(document, path);
      if (current !== undefined && typeof current !== "number") {
        throw new GuardianDBError("INVALID_UPDATE", `$inc target '${path}' must be numeric`);
      }
      changed = setPath(document, path, (current ?? 0) + increment) || changed;
    }
  }

  for (const key of keys) {
    if (key !== "$set" && key !== "$unset" && key !== "$inc") {
      throw new GuardianDBError("INVALID_UPDATE", `Unsupported update operator '${key}'`);
    }
  }
  return changed;
}

function matchesCondition(actual: unknown, condition: unknown): boolean {
  if (isOperatorObject(condition)) {
    for (const [operator, operand] of Object.entries(condition)) {
      switch (operator) {
        case "$eq":
          if (!equalWithArraySemantics(actual, operand)) return false;
          break;
        case "$ne":
          if (equalWithArraySemantics(actual, operand)) return false;
          break;
        case "$gt":
          if (!(compare(actual, operand) > 0)) return false;
          break;
        case "$gte":
          if (!(compare(actual, operand) >= 0)) return false;
          break;
        case "$lt":
          if (!(compare(actual, operand) < 0)) return false;
          break;
        case "$lte":
          if (!(compare(actual, operand) <= 0)) return false;
          break;
        case "$in":
          if (!Array.isArray(operand)) throw invalidOperand("$in", "an array");
          if (!operand.some((candidate) => equalWithArraySemantics(actual, candidate))) return false;
          break;
        case "$nin":
          if (!Array.isArray(operand)) throw invalidOperand("$nin", "an array");
          if (operand.some((candidate) => equalWithArraySemantics(actual, candidate))) return false;
          break;
        case "$exists":
          if (typeof operand !== "boolean") throw invalidOperand("$exists", "a boolean");
          if ((actual !== undefined) !== operand) return false;
          break;
        case "$size":
          if (typeof operand !== "number" || !Number.isInteger(operand) || operand < 0) {
            throw invalidOperand("$size", "a non-negative integer");
          }
          if (!Array.isArray(actual) || actual.length !== operand) return false;
          break;
        default:
          throw new GuardianDBError("INVALID_QUERY", `Unsupported field operator '${operator}'`);
      }
    }
    return true;
  }
  return equalWithArraySemantics(actual, condition);
}

function isOperatorObject(value: unknown): value is Record<string, unknown> {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).some((key) => key.startsWith("$"))
  );
}

function equalWithArraySemantics(actual: unknown, expected: unknown): boolean {
  return deepEqual(actual, expected) || (Array.isArray(actual) && actual.some((value) => deepEqual(value, expected)));
}

function compare(left: unknown, right: unknown): number {
  if (typeof left === "number" && typeof right === "number") return left - right;
  if (typeof left === "string" && typeof right === "string") return left.localeCompare(right);
  return Number.NaN;
}

function invalidOperand(operator: string, expected: string): GuardianDBError {
  return new GuardianDBError("INVALID_QUERY", `${operator} expects ${expected}`);
}

function assertMutable(path: string, immutableFields: ReadonlySet<string>): void {
  for (const field of immutableFields) {
    if (path === field || path.startsWith(`${field}.`)) {
      throw new GuardianDBError("IMMUTABLE_FIELD", `Immutable field '${field}' cannot be updated`, {
        field,
      });
    }
  }
}
