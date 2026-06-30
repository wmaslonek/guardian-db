/**
 * `@guardiandb/postgres-typeorm` — a GuardianDB-aware TypeORM DataSource.
 *
 * `GuardianDataSource` is a thin, optional convenience over TypeORM's standard
 * PostgreSQL driver: it manages an embedded GuardianDB PostgreSQL gateway for
 * you and then behaves exactly like a normal `DataSource` (entities,
 * repositories, migrations, schema sync, QueryBuilder, transactions and
 * relation metadata all work, because the underlying driver *is* `postgres`).
 *
 * The PostgreSQL wire path (`type: "postgres"`) remains the primary, required
 * way to use GuardianDB from TypeORM; this package is sugar for embedding.
 *
 * ```ts
 * const ds = new GuardianDataSource({
 *   path: "./data",
 *   database: "app",
 *   peers: [],
 *   consistency: "strict",
 *   entities: [User, Post, Org],
 * });
 * await ds.initialize();
 * ```
 */

import { spawn, ChildProcess } from "node:child_process";
import net from "node:net";
import fs from "node:fs";
import path from "node:path";
import { DataSource, DataSourceOptions, EntitySchema, MixedList } from "typeorm";

export type Consistency = "local" | "strict";

export interface GuardianDataSourceOptions {
  /** Local data directory for the GuardianDB node (GuardianDB-backed gateway). */
  path?: string;
  /** Logical database name. */
  database?: string;
  /** Iroh peer addresses to replicate with. */
  peers?: string[];
  /** Consistency mode: local-first (default) or strict SQL. */
  consistency?: Consistency;
  /** TCP port for the embedded gateway (default 15432). */
  port?: number;
  /** Bind host (default 127.0.0.1). */
  host?: string;
  username?: string;
  password?: string;
  /** Path to the `guardian-pgwire` binary (else resolved from env/`target`). */
  binary?: string;
  entities?: MixedList<Function | string | EntitySchema>;
  migrations?: MixedList<Function | string>;
  synchronize?: boolean;
  logging?: DataSourceOptions["logging"];
}

function resolveBinary(explicit?: string): string {
  if (explicit) return explicit;
  if (process.env.GUARDIAN_PGWIRE_BIN) return process.env.GUARDIAN_PGWIRE_BIN;
  // Walk up looking for a built binary (useful inside the monorepo).
  let dir = process.cwd();
  for (let i = 0; i < 6; i++) {
    for (const rel of ["target/debug/guardian-pgwire", "target/release/guardian-pgwire"]) {
      const full = path.join(dir, rel);
      if (fs.existsSync(full)) return full;
    }
    dir = path.dirname(dir);
  }
  throw new Error(
    "guardian-pgwire binary not found. Set GUARDIAN_PGWIRE_BIN or pass { binary }.",
  );
}

function waitForPort(host: string, port: number, timeoutMs = 10_000): Promise<void> {
  const start = Date.now();
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const s = net.connect(port, host);
      s.on("connect", () => {
        s.destroy();
        resolve();
      });
      s.on("error", () => {
        s.destroy();
        if (Date.now() - start > timeoutMs) reject(new Error("gateway did not start"));
        else setTimeout(attempt, 120);
      });
    };
    attempt();
  });
}

export class GuardianDataSource extends DataSource {
  private gateway?: ChildProcess;
  private readonly guardian: Required<Pick<GuardianDataSourceOptions, "host" | "port" | "database" | "consistency">> &
    GuardianDataSourceOptions;

  constructor(opts: GuardianDataSourceOptions) {
    const host = opts.host ?? "127.0.0.1";
    const port = opts.port ?? 15432;
    const database = opts.database ?? "app";
    const username = opts.username ?? "guardian";
    const password = opts.password ?? "guardian";

    super({
      type: "postgres",
      host,
      port,
      username,
      password,
      database,
      entities: opts.entities ?? [],
      migrations: opts.migrations ?? [],
      synchronize: opts.synchronize ?? false,
      logging: opts.logging ?? ["error"],
    } as DataSourceOptions);

    this.guardian = { ...opts, host, port, database, consistency: opts.consistency ?? "local" };
  }

  /** Start the embedded gateway, then initialize the TypeORM DataSource. */
  override async initialize(): Promise<this> {
    const bin = resolveBinary(this.guardian.binary);
    const args = ["--addr", `${this.guardian.host}:${this.guardian.port}`, "--database", this.guardian.database];
    if (this.guardian.path) args.push("--path", this.guardian.path);
    if (this.guardian.consistency) args.push("--consistency", this.guardian.consistency);
    for (const peer of this.guardian.peers ?? []) args.push("--peer", peer);

    this.gateway = spawn(bin, args, { stdio: "ignore" });
    this.gateway.on("error", (e) => {
      throw e;
    });
    await waitForPort(this.guardian.host, this.guardian.port);
    await super.initialize();
    return this;
  }

  /** Tear down the TypeORM DataSource and stop the embedded gateway. */
  override async destroy(): Promise<void> {
    try {
      await super.destroy();
    } finally {
      this.gateway?.kill("SIGKILL");
      this.gateway = undefined;
    }
  }
}

export default GuardianDataSource;
