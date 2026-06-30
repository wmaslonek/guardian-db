import { Collection } from "./collection.js";
import { GuardianDBError } from "./errors.js";
import { MemoryTransport } from "./memory-transport.js";
import { normalizeSchema } from "./schema.js";
import { transportFromIroh, type DatabaseReference, type GuardianTransport } from "./transport.js";
import type { CollectionOptions, Document, GuardianDBInitOptions } from "./types.js";

export default class GuardianDB {
  private static readonly transports = new Set<GuardianTransport>([MemoryTransport.shared]);
  private readonly collections = new Map<string, Collection>();

  private constructor(
    private readonly database: DatabaseReference,
    private readonly transport: GuardianTransport,
  ) {}

  public static async init(
    databaseName: string,
    iroh: unknown,
    options: GuardianDBInitOptions = {},
  ): Promise<GuardianDB> {
    if (databaseName.trim().length === 0) {
      throw new GuardianDBError("INVALID_DATABASE_NAME", "Database name cannot be empty");
    }
    const transport = options.transport ?? transportFromIroh(iroh) ?? MemoryTransport.shared;
    const database: DatabaseReference = {
      name: databaseName,
      path: options.path ?? "./.guardiandb",
    };
    await transport.initDatabase(database);
    GuardianDB.transports.add(transport);
    return new GuardianDB(database, transport);
  }

  public static async listDatabases(): Promise<string[]> {
    const names = new Set<string>();
    for (const transport of GuardianDB.transports) {
      for (const name of await transport.listDatabases()) names.add(name);
    }
    return [...names].sort();
  }

  public async initCollection<T extends Document = Document>(
    collectionName: string,
    options: CollectionOptions<T> = {},
  ): Promise<Collection<T>> {
    if (collectionName.trim().length === 0) {
      throw new GuardianDBError("INVALID_COLLECTION_NAME", "Collection name cannot be empty");
    }
    const existing = this.collections.get(collectionName);
    if (existing !== undefined && options.schema === undefined) {
      return existing as Collection<T>;
    }

    const schema = normalizeSchema(options.schema);
    await this.transport.initCollection(this.database, collectionName, schema);
    const collection = new Collection<T>(this.database, collectionName, schema, this.transport);
    this.collections.set(collectionName, collection);
    return collection;
  }

  public listCollections(): Promise<string[]> {
    return this.transport.listCollections(this.database);
  }
}
