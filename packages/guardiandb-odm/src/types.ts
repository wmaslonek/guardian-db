export type Document = Record<string, unknown>;
export type DocumentId = string | number | boolean;

export type ScalarFieldType =
  | "any"
  | "string"
  | "number"
  | "boolean"
  | "object"
  | "array"
  | "timestamp";

export interface FieldSchema<T = unknown> {
  type?: ScalarFieldType | StringConstructor | NumberConstructor | BooleanConstructor | ObjectConstructor | ArrayConstructor;
  required?: boolean;
  nullable?: boolean;
  primaryKey?: boolean;
  unique?: boolean;
  index?: boolean;
  default?: T | (() => T);
  validate?: (value: T, document: Readonly<Document>) => boolean | string;
}

export type SchemaFields<T extends Document> = {
  [K in keyof T]?: FieldSchema<T[K]>;
} & Record<string, FieldSchema>;

export interface TimestampOptions {
  createdAt?: string;
  updatedAt?: string;
}

export interface SchemaDefinition<T extends Document = Document> {
  fields?: SchemaFields<T>;
  strict?: boolean;
  timestamps?: boolean | TimestampOptions;
  version?: number;
}

export interface NormalizedFieldSchema {
  type: ScalarFieldType;
  required: boolean;
  nullable: boolean;
  primaryKey: boolean;
  unique: boolean;
  index: boolean;
  default?: unknown | (() => unknown);
  validate?: (value: unknown, document: Readonly<Document>) => boolean | string;
}

export interface NormalizedSchema {
  fields: Record<string, NormalizedFieldSchema>;
  primaryKey: string;
  strict: boolean;
  timestamps?: Required<TimestampOptions>;
  version: number;
}

export interface ComparisonOperators<T = unknown> {
  $eq?: T;
  $ne?: T;
  $gt?: T;
  $gte?: T;
  $lt?: T;
  $lte?: T;
  $in?: T[];
  $nin?: T[];
  $exists?: boolean;
  $size?: number;
}

export type QueryValue<T> = T extends readonly (infer Element)[] ? T | Element : T;

export type FieldQuery<T> = QueryValue<T> | ComparisonOperators<QueryValue<T>>;

export type Query<T extends Document = Document> = {
  [K in keyof T]?: FieldQuery<T[K]>;
} & {
  [path: string]: unknown;
  $and?: Query<T>[];
  $or?: Query<T>[];
  $nor?: Query<T>[];
};

export interface UpdateOperations<T extends Document = Document> {
  $set?: Partial<T> & Record<string, unknown>;
  $unset?: Partial<Record<keyof T | string, unknown>>;
  $inc?: Partial<Record<keyof T | string, number>>;
}

export type ConsistencyLevel = "local_atomic" | "replicated";

export interface TransactionContext {
  id: string;
  startedAt: string;
  consistency: ConsistencyLevel;
}

export interface WriteOptions {
  transaction?: TransactionContext;
}

export interface GuardianDBInitOptions {
  path?: string;
  transport?: import("./transport.js").GuardianTransport;
}

export interface CollectionOptions<T extends Document = Document> {
  schema?: SchemaDefinition<T>;
}

export interface CollectionDescriptor {
  name: string;
  schema: NormalizedSchema;
}
