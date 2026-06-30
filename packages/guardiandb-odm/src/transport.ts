import type {
  Document,
  DocumentId,
  NormalizedSchema,
  Query,
  UpdateOperations,
  WriteOptions,
} from "./types.js";

export interface DatabaseReference {
  name: string;
  path: string;
}

/**
 * Native/storage boundary for the TypeScript ODM.
 *
 * A Node, browser, React Native, or WASM binding can implement this interface
 * without coupling the high-level collection API to a specific Iroh runtime.
 */
export interface GuardianTransport {
  initDatabase(database: DatabaseReference): Promise<void>;
  listDatabases(): Promise<string[]>;
  initCollection(database: DatabaseReference, name: string, schema: NormalizedSchema): Promise<void>;
  listCollections(database: DatabaseReference): Promise<string[]>;
  insertOne<T extends Document>(
    database: DatabaseReference,
    collection: string,
    document: T,
    options?: WriteOptions,
  ): Promise<T>;
  insert<T extends Document>(
    database: DatabaseReference,
    collection: string,
    documents: readonly T[],
    options?: WriteOptions,
  ): Promise<T[]>;
  findOne<T extends Document>(
    database: DatabaseReference,
    collection: string,
    query: Query<T>,
  ): Promise<T | null>;
  find<T extends Document>(
    database: DatabaseReference,
    collection: string,
    query: Query<T>,
  ): Promise<T[]>;
  findById<T extends Document>(
    database: DatabaseReference,
    collection: string,
    id: DocumentId,
  ): Promise<T | null>;
  update<T extends Document>(
    database: DatabaseReference,
    collection: string,
    query: Query<T>,
    operations: UpdateOperations<T>,
    options?: WriteOptions,
  ): Promise<T | null>;
}

export interface GuardianTransportProvider {
  guardianDBTransport?: GuardianTransport;
  guardianDbTransport?: GuardianTransport;
}

export function transportFromIroh(iroh: unknown): GuardianTransport | undefined {
  if (iroh === null || typeof iroh !== "object") {
    return undefined;
  }
  const provider = iroh as GuardianTransportProvider;
  return provider.guardianDBTransport ?? provider.guardianDbTransport;
}
