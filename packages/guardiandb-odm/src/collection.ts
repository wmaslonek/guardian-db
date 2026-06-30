import type { DatabaseReference, GuardianTransport } from "./transport.js";
import type {
  Document,
  DocumentId,
  NormalizedSchema,
  Query,
  TransactionContext,
  UpdateOperations,
  WriteOptions,
} from "./types.js";
import { randomId } from "./utils.js";

export class Collection<T extends Document = Document> {
  public readonly name: string;
  public readonly schema: Readonly<NormalizedSchema>;

  public constructor(
    private readonly database: DatabaseReference,
    name: string,
    schema: NormalizedSchema,
    private readonly transport: GuardianTransport,
  ) {
    this.name = name;
    this.schema = schema;
  }

  public insertOne(document: T, options?: WriteOptions): Promise<T> {
    return this.transport.insertOne(this.database, this.name, document, options);
  }

  public insert(documents: readonly T[], options?: WriteOptions): Promise<T[]> {
    return this.transport.insert(this.database, this.name, documents, options);
  }

  public findOne(query: Query<T> = {} as Query<T>): Promise<T | null> {
    return this.transport.findOne(this.database, this.name, query);
  }

  public find(query: Query<T> = {} as Query<T>): Promise<T[]> {
    return this.transport.find(this.database, this.name, query);
  }

  public findById(id: DocumentId): Promise<T | null> {
    return this.transport.findById(this.database, this.name, id);
  }

  public update(
    query: Query<T>,
    operations: UpdateOperations<T>,
    options?: WriteOptions,
  ): Promise<T | null> {
    return this.transport.update(this.database, this.name, query, operations, options);
  }

  public beginTransaction(consistency: TransactionContext["consistency"] = "local_atomic"): TransactionContext {
    return {
      id: randomId(),
      startedAt: new Date().toISOString(),
      consistency,
    };
  }
}
