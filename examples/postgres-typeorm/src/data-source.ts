import "reflect-metadata";
import { DataSource, DataSourceOptions } from "typeorm";
import { Org } from "./entities/Org";
import { User } from "./entities/User";
import { Post } from "./entities/Post";
import { Init1700000000000 } from "./migrations/1700000000000-Init";

/**
 * Standard TypeORM PostgreSQL configuration pointed at the GuardianDB gateway.
 *
 * This is exactly what you would write against a real PostgreSQL server — the
 * gateway speaks the PostgreSQL wire protocol, so `type: "postgres"` is all that
 * is required.
 */
export function options(overrides: Partial<DataSourceOptions> = {}): DataSourceOptions {
  return {
    type: "postgres",
    host: process.env.PGHOST ?? "127.0.0.1",
    port: Number(process.env.PGPORT ?? 15432),
    username: process.env.PGUSER ?? "guardian",
    password: process.env.PGPASSWORD ?? "guardian",
    database: process.env.PGDATABASE ?? "app",
    entities: [Org, User, Post],
    migrations: [Init1700000000000],
    // Prefer migrations in real apps; synchronize is convenient for demos.
    synchronize: false,
    logging: ["error", "warn"],
    ...overrides,
  } as DataSourceOptions;
}

/** The DataSource used by the TypeORM migration CLI. */
export const AppDataSource = new DataSource(options());
