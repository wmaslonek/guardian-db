import { DuplicateKeyError, GuardianDBError } from "./errors.js";
import { applyUpdate, matchesQuery } from "./query.js";
import { prepareInsert, touchUpdatedAt, validateDocument } from "./schema.js";
import { canonicalId, clone, getPath, indexToken } from "./utils.js";
import type { DatabaseReference, GuardianTransport } from "./transport.js";
import type {
  Document,
  DocumentId,
  NormalizedSchema,
  Query,
  UpdateOperations,
  WriteOptions,
} from "./types.js";

type Index = Map<string, Set<string>>;

interface IndexedValue {
  value: unknown;
  tokens: ReadonlySet<string>;
}

type DocumentIndexEntries = Map<string, IndexedValue>;

interface IndexCatalog {
  byField: Map<string, Index>;
}

interface MemoryCollection {
  schema: NormalizedSchema;
  documents: Map<string, Document>;
  indexes: IndexCatalog;
  mutex: AsyncMutex;
}

interface MemoryDatabase {
  name: string;
  collections: Map<string, MemoryCollection>;
}

/**
 * Process-local reference transport. It makes the SDK directly executable in
 * tests and development while native Iroh/WASM bindings implement the same
 * GuardianTransport interface for decentralized persistence.
 */
export class MemoryTransport implements GuardianTransport {
  public static readonly shared = new MemoryTransport();

  private readonly databases = new Map<string, MemoryDatabase>();

  public async initDatabase(database: DatabaseReference): Promise<void> {
    const key = databaseKey(database);
    if (!this.databases.has(key)) {
      this.databases.set(key, { name: database.name, collections: new Map() });
    }
  }

  public async listDatabases(): Promise<string[]> {
    return [...new Set([...this.databases.values()].map((database) => database.name))].sort();
  }

  public async initCollection(
    database: DatabaseReference,
    name: string,
    schema: NormalizedSchema,
  ): Promise<void> {
    const db = this.database(database);
    const existing = db.collections.get(name);
    if (existing !== undefined) {
      if (!schemasCompatible(existing.schema, schema)) {
        throw new GuardianDBError(
          "SCHEMA_MISMATCH",
          `Collection '${name}' was already initialized with a different schema`,
        );
      }
      return;
    }
    db.collections.set(name, {
      schema,
      documents: new Map(),
      indexes: emptyIndexes(schema),
      mutex: new AsyncMutex(),
    });
  }

  public async listCollections(database: DatabaseReference): Promise<string[]> {
    return [...this.database(database).collections.keys()].sort();
  }

  public async insertOne<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    input: T,
    options?: WriteOptions,
  ): Promise<T> {
    const inserted = await this.insert(database, collectionName, [input], options);
    return inserted[0]!;
  }

  public async insert<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    inputs: readonly T[],
    options?: WriteOptions,
  ): Promise<T[]> {
    assertLocalWrite(options);
    const collection = this.collection(database, collectionName);
    return collection.mutex.runExclusive(async () => {
      const prepared: Array<{
        id: string;
        document: Document;
        indexEntries: DocumentIndexEntries;
        result: T;
      }> = [];
      const reservedIds = new Set<string>();
      const uniqueReservations = new Map<string, Map<string, string>>();

      for (const input of inputs) {
        const document = prepareInsert(collection.schema, input);
        const id = canonicalId(document[collection.schema.primaryKey]);
        if (collection.documents.has(id) || reservedIds.has(id)) {
          throw new DuplicateKeyError(collection.schema.primaryKey, document[collection.schema.primaryKey]);
        }

        const indexEntries = collectIndexEntries(collection.indexes, document);
        assertUniqueIndexEntries(
          collection.schema,
          collection.indexes,
          indexEntries,
          id,
          uniqueReservations,
        );
        reservedIds.add(id);
        prepared.push({
          id,
          document,
          indexEntries,
          result: clone(document) as T,
        });
      }

      // Everything that can fail is completed before mutating collection state.
      // The commit itself consists only of in-memory Map/Set writes, preserving
      // the batch's all-or-nothing behavior without cloning the entire dataset.
      for (const item of prepared) {
        collection.documents.set(item.id, item.document);
        addIndexEntries(collection.indexes, item.id, item.indexEntries);
      }
      return prepared.map((item) => item.result);
    });
  }

  public async findOne<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    query: Query<T>,
  ): Promise<T | null> {
    const collection = this.collection(database, collectionName);
    return collection.mutex.runExclusive(async () => {
      const candidates = candidateIds(collection.indexes, query);
      const ids: Iterable<string> = candidates ?? collection.documents.keys();
      for (const id of ids) {
        const document = collection.documents.get(id);
        if (document !== undefined && matchesQuery(document, query as Query<Document>)) {
          return clone(document) as T;
        }
      }
      return null;
    });
  }

  public async find<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    query: Query<T>,
  ): Promise<T[]> {
    const collection = this.collection(database, collectionName);
    return collection.mutex.runExclusive(async () => {
      const candidates = candidateIds(collection.indexes, query);
      const ids: Iterable<string> = candidates ?? collection.documents.keys();

      const results: T[] = [];
      for (const id of ids) {
        const document = collection.documents.get(id);
        if (document !== undefined && matchesQuery(document, query as Query<Document>)) {
          results.push(clone(document) as T);
        }
      }
      return results;
    });
  }

  public async findById<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    id: DocumentId,
  ): Promise<T | null> {
    const collection = this.collection(database, collectionName);
    return collection.mutex.runExclusive(async () => {
      const document = collection.documents.get(canonicalId(id));
      return document === undefined ? null : (clone(document) as T);
    });
  }

  public async update<T extends Document>(
    database: DatabaseReference,
    collectionName: string,
    query: Query<T>,
    operations: UpdateOperations<T>,
    options?: WriteOptions,
  ): Promise<T | null> {
    assertLocalWrite(options);
    const collection = this.collection(database, collectionName);
    return collection.mutex.runExclusive(async () => {
      const candidates = candidateIds(collection.indexes, query);
      const ids: Iterable<string> = candidates ?? collection.documents.keys();
      let matchedId: string | undefined;
      for (const id of ids) {
        const document = collection.documents.get(id);
        if (document !== undefined && matchesQuery(document, query as Query<Document>)) {
          matchedId = id;
          break;
        }
      }
      if (matchedId === undefined) return null;

      const updated = clone(collection.documents.get(matchedId)!);
      const changed = applyUpdate(updated, operations as UpdateOperations<Document>, new Set([
        collection.schema.primaryKey,
        "_id",
      ]));
      if (!changed) return clone(updated) as T;

      touchUpdatedAt(collection.schema, updated);
      validateDocument(collection.schema, updated);
      const updatedId = canonicalId(updated[collection.schema.primaryKey]);
      if (updatedId !== matchedId) {
        throw new GuardianDBError(
          "IMMUTABLE_FIELD",
          `Immutable field '${collection.schema.primaryKey}' cannot be updated`,
        );
      }

      const previous = collection.documents.get(matchedId)!;
      const previousIndexEntries = collectIndexEntries(collection.indexes, previous);
      const updatedIndexEntries = collectIndexEntries(collection.indexes, updated);
      assertUniqueIndexEntries(
        collection.schema,
        collection.indexes,
        updatedIndexEntries,
        matchedId,
      );

      // Validation and unique checks happen before this commit. Updating just
      // the affected document and index entries avoids O(collection size) work
      // for every single-document update.
      removeIndexEntries(collection.indexes, matchedId, previousIndexEntries);
      collection.documents.set(matchedId, updated);
      addIndexEntries(collection.indexes, matchedId, updatedIndexEntries);
      return clone(updated) as T;
    });
  }

  /** Clears all process-local state; intended for tests. */
  public reset(): void {
    this.databases.clear();
  }

  private database(reference: DatabaseReference): MemoryDatabase {
    const database = this.databases.get(databaseKey(reference));
    if (database === undefined) {
      throw new GuardianDBError("DATABASE_NOT_INITIALIZED", `Database '${reference.name}' is not initialized`);
    }
    return database;
  }

  private collection(reference: DatabaseReference, name: string): MemoryCollection {
    const collection = this.database(reference).collections.get(name);
    if (collection === undefined) {
      throw new GuardianDBError("COLLECTION_NOT_INITIALIZED", `Collection '${name}' is not initialized`);
    }
    return collection;
  }
}

class AsyncMutex {
  private tail: Promise<void> = Promise.resolve();

  public async runExclusive<T>(operation: () => Promise<T>): Promise<T> {
    let release!: () => void;
    const next = new Promise<void>((resolve) => {
      release = resolve;
    });
    const previous = this.tail;
    this.tail = previous.then(() => next);
    await previous;
    try {
      return await operation();
    } finally {
      release();
    }
  }
}

function emptyIndexes(schema: NormalizedSchema): IndexCatalog {
  const byField = new Map<string, Index>();
  for (const [field, definition] of Object.entries(schema.fields)) {
    if (definition.index || definition.unique || definition.primaryKey) {
      byField.set(field, new Map());
    }
  }
  return { byField };
}

function collectIndexEntries(catalog: IndexCatalog, document: Document): DocumentIndexEntries {
  const entries: DocumentIndexEntries = new Map();
  for (const field of catalog.byField.keys()) {
    const value = getPath(document, field);
    if (value === undefined || value === null) continue;
    entries.set(field, { value, tokens: indexTokens(value) });
  }
  return entries;
}

function assertUniqueIndexEntries(
  schema: NormalizedSchema,
  catalog: IndexCatalog,
  entries: DocumentIndexEntries,
  documentId: string,
  reservations?: Map<string, Map<string, string>>,
): void {
  for (const [field, indexedValue] of entries) {
    const definition = schema.fields[field];
    if (definition === undefined || (!definition.unique && !definition.primaryKey)) continue;

    let fieldReservations: Map<string, string> | undefined;
    if (reservations !== undefined) {
      fieldReservations = reservations.get(field);
      if (fieldReservations === undefined) {
        fieldReservations = new Map();
        reservations.set(field, fieldReservations);
      }
    }

    const index = catalog.byField.get(field)!;
    for (const token of indexedValue.tokens) {
      const indexedIds = index.get(token);
      if (indexedIds !== undefined && [...indexedIds].some((id) => id !== documentId)) {
        throw new DuplicateKeyError(field, indexedValue.value);
      }

      const reservedId = fieldReservations?.get(token);
      if (reservedId !== undefined && reservedId !== documentId) {
        throw new DuplicateKeyError(field, indexedValue.value);
      }
      fieldReservations?.set(token, documentId);
    }
  }
}

function addIndexEntries(catalog: IndexCatalog, documentId: string, entries: DocumentIndexEntries): void {
  for (const [field, indexedValue] of entries) {
    const index = catalog.byField.get(field)!;
    for (const token of indexedValue.tokens) {
      let ids = index.get(token);
      if (ids === undefined) {
        ids = new Set();
        index.set(token, ids);
      }
      ids.add(documentId);
    }
  }
}

function removeIndexEntries(catalog: IndexCatalog, documentId: string, entries: DocumentIndexEntries): void {
  for (const [field, indexedValue] of entries) {
    const index = catalog.byField.get(field)!;
    for (const token of indexedValue.tokens) {
      const ids = index.get(token);
      if (ids === undefined) continue;
      ids.delete(documentId);
      if (ids.size === 0) index.delete(token);
    }
  }
}

function candidateIds<T extends Document>(catalog: IndexCatalog, query: Query<T>): Set<string> | undefined {
  let candidates: Set<string> | undefined;
  for (const [field, condition] of Object.entries(query)) {
    const index = catalog.byField.get(field);
    if (index === undefined) continue;
    const equality = equalityOperand(condition);
    if (equality === NO_EQUALITY || equality === null || equality === undefined) continue;
    const matches = index.get(indexToken(equality)) ?? new Set<string>();
    candidates = candidates === undefined
      ? new Set(matches)
      : new Set([...candidates].filter((id) => matches.has(id)));
  }
  return candidates;
}

const NO_EQUALITY = Symbol("no equality");

function equalityOperand(value: unknown): unknown | typeof NO_EQUALITY {
  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    const entries = Object.entries(value);
    if (entries.length === 1 && entries[0]?.[0] === "$eq") {
      return entries[0][1];
    }
    if (entries.some(([key]) => key.startsWith("$"))) return NO_EQUALITY;
  }
  return value;
}

function indexTokens(value: unknown): Set<string> {
  const tokens = new Set([indexToken(value)]);
  if (Array.isArray(value)) {
    for (const item of value) tokens.add(indexToken(item));
  }
  return tokens;
}

function databaseKey(database: DatabaseReference): string {
  return `${database.path}\u0000${database.name}`;
}

function schemasCompatible(left: NormalizedSchema, right: NormalizedSchema): boolean {
  const serializable = (schema: NormalizedSchema): unknown => ({
    ...schema,
    fields: Object.fromEntries(
      Object.entries(schema.fields).map(([name, field]) => [
        name,
        { ...field, default: typeof field.default === "function" ? "[function]" : field.default, validate: field.validate === undefined ? undefined : "[function]" },
      ]),
    ),
  });
  return JSON.stringify(serializable(left)) === JSON.stringify(serializable(right));
}

function assertLocalWrite(options?: WriteOptions): void {
  if (options?.transaction?.consistency === "replicated") {
    throw new GuardianDBError(
      "UNSUPPORTED_CONSISTENCY",
      "Replicated transactions require a future distributed coordinator; local_atomic is available today",
    );
  }
}
