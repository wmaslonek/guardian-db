export { default } from "./guardian-db.js";
export { default as GuardianDB } from "./guardian-db.js";
export { Collection } from "./collection.js";
export { MemoryTransport } from "./memory-transport.js";
export { defineSchema } from "./schema.js";
export {
  DuplicateKeyError,
  GuardianDBError,
  ValidationError,
} from "./errors.js";
export type {
  DatabaseReference,
  GuardianTransport,
  GuardianTransportProvider,
} from "./transport.js";
export type {
  CollectionDescriptor,
  CollectionOptions,
  ComparisonOperators,
  ConsistencyLevel,
  Document,
  DocumentId,
  FieldQuery,
  FieldSchema,
  GuardianDBInitOptions,
  NormalizedSchema,
  Query,
  QueryValue,
  SchemaDefinition,
  SchemaFields,
  TimestampOptions,
  TransactionContext,
  UpdateOperations,
  WriteOptions,
} from "./types.js";
