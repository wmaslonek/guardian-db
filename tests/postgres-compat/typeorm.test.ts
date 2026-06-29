// Conformance tests driving TypeORM (type: "postgres") against the gateway:
// DataSource init, schema synchronize + re-introspection, repository &
// EntityManager APIs, QueryBuilder, transactions, relations, unique
// constraints, JSONB, timestamps, generated ids, and migrations.

import "reflect-metadata";
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import {
  DataSource, Entity, PrimaryGeneratedColumn, Column, Index,
  ManyToOne, OneToMany, CreateDateColumn, MigrationInterface, QueryRunner,
  Table, TableIndex,
} from "typeorm";
import { startGateway, Gateway } from "./harness";

@Entity()
class Org {
  @PrimaryGeneratedColumn() id!: number;
  @Column({ type: "text" }) name!: string;
  @OneToMany(() => User, (u) => u.org) users!: User[];
}

@Entity()
class User {
  @PrimaryGeneratedColumn() id!: number;
  @Index({ unique: true }) @Column({ type: "varchar", length: 160 }) email!: string;
  @Column({ type: "text" }) name!: string;
  @Column({ type: "jsonb", nullable: true }) prefs!: any;
  @ManyToOne(() => Org, (o) => o.users, { nullable: true }) org!: Org | null;
  @OneToMany(() => Post, (p) => p.author) posts!: Post[];
  @CreateDateColumn({ type: "timestamptz" }) createdAt!: Date;
}

@Entity()
class Post {
  @PrimaryGeneratedColumn("uuid") id!: string;
  @Column({ type: "text" }) title!: string;
  @Column({ type: "jsonb", default: {} }) meta!: any;
  @ManyToOne(() => User, (u) => u.posts) author!: User;
}

let gw: Gateway;
const ENTITIES = [Org, User, Post];

function makeDataSource(synchronize = true) {
  return new DataSource({
    type: "postgres",
    host: "127.0.0.1",
    port: gw.port,
    username: "guardian",
    password: "guardian",
    database: "app",
    synchronize,
    entities: ENTITIES,
    logging: ["error"],
  });
}

before(async () => {
  gw = await startGateway();
});
after(() => gw?.stop());

test("initialize + synchronize creates the schema", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  assert.ok(ds.isInitialized);
  // The tables exist and are introspectable.
  const tables = await ds.query(
    "SELECT table_name FROM information_schema.tables WHERE table_schema='public' ORDER BY table_name",
  );
  const names = tables.map((t: any) => t.table_name);
  assert.ok(names.includes("user"));
  assert.ok(names.includes("org"));
  assert.ok(names.includes("post"));
  await ds.destroy();
});

test("repository CRUD, generated ids, jsonb, timestamps", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  const users = ds.getRepository(User);

  const u = await users.save(users.create({ email: "a@x.com", name: "Alice", prefs: { theme: "dark" } }));
  assert.equal(typeof u.id, "number");
  assert.ok(u.createdAt instanceof Date);

  const found = await users.findOneBy({ email: "a@x.com" });
  assert.equal(found?.name, "Alice");
  assert.deepEqual(found?.prefs, { theme: "dark" });

  await users.update({ id: u.id }, { name: "Alice II" });
  assert.equal((await users.findOneBy({ id: u.id }))?.name, "Alice II");

  const all = await users.find({ where: { name: "Alice II" } });
  assert.equal(all.length, 1);

  await users.delete({ id: u.id });
  assert.equal(await users.count(), 0);
  await ds.destroy();
});

test("unique constraint is enforced", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  const users = ds.getRepository(User);
  await users.save(users.create({ email: "dup@x.com", name: "A" }));
  await assert.rejects(
    () => users.save(users.create({ email: "dup@x.com", name: "B" })),
    (e: any) => String(e.code) === "23505" || /unique/i.test(String(e.message)),
  );
  await ds.destroy();
});

test("relations: save and load OneToMany / ManyToOne", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  const org = await ds.getRepository(Org).save({ name: "Acme" } as Org);
  const user = await ds.getRepository(User).save({ email: "rel@x.com", name: "Rel", org } as any);
  await ds.getRepository(Post).save([
    { title: "First", author: user, meta: { n: 1 } } as any,
    { title: "Second", author: user, meta: {} } as any,
  ]);
  const loaded = await ds.getRepository(User).findOne({
    where: { email: "rel@x.com" },
    relations: { org: true, posts: true },
  });
  assert.equal(loaded?.org?.name, "Acme");
  assert.equal(loaded?.posts.length, 2);
  await ds.destroy();
});

test("QueryBuilder joins, where, order", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  const user = await ds.getRepository(User).save({ email: "qb@x.com", name: "QB" } as any);
  await ds.getRepository(Post).save([
    { title: "B", author: user, meta: {} } as any,
    { title: "A", author: user, meta: {} } as any,
  ]);
  const rows = await ds
    .getRepository(Post)
    .createQueryBuilder("p")
    .innerJoinAndSelect("p.author", "u")
    .where("u.email = :e", { e: "qb@x.com" })
    .orderBy("p.title", "ASC")
    .getMany();
  assert.deepEqual(rows.map((r) => r.title), ["A", "B"]);
  assert.equal(rows[0].author.name, "QB");
  await ds.destroy();
});

test("EntityManager transaction commits atomically", async () => {
  const ds = makeDataSource();
  await ds.initialize();
  const before = await ds.getRepository(User).count();
  await ds.transaction(async (m) => {
    const u = await m.getRepository(User).save({ email: "tx@x.com", name: "Tx" } as any);
    await m.getRepository(Post).save({ title: "TxPost", author: u, meta: {} } as any);
  });
  assert.equal(await ds.getRepository(User).count(), before + 1);
  await ds.destroy();
});

test("schema re-synchronize against existing schema is a no-op", async () => {
  // First DataSource creates & populates.
  let ds = makeDataSource();
  await ds.initialize();
  await ds.getRepository(User).save({ email: "persist@x.com", name: "P" } as any);
  await ds.destroy();
  // Second DataSource synchronizes against the existing (introspected) schema.
  ds = makeDataSource();
  await ds.initialize();
  assert.ok((await ds.getRepository(User).count()) >= 1);
  await ds.destroy();
});

class CreateAudit1700000000000 implements MigrationInterface {
  name = "CreateAudit1700000000000";
  async up(q: QueryRunner): Promise<void> {
    await q.createTable(
      new Table({
        name: "audit",
        columns: [
          { name: "id", type: "int", isPrimary: true, isGenerated: true, generationStrategy: "increment" },
          { name: "action", type: "text", isNullable: false },
          { name: "at", type: "timestamptz", default: "now()" },
        ],
      }),
      true,
    );
    await q.createIndex("audit", new TableIndex({ name: "idx_audit_action", columnNames: ["action"] }));
  }
  async down(q: QueryRunner): Promise<void> {
    await q.dropTable("audit");
  }
}

test("migrations run and are idempotent", async () => {
  const ds = new DataSource({
    type: "postgres",
    host: "127.0.0.1",
    port: gw.port,
    username: "guardian",
    password: "guardian",
    database: "app",
    synchronize: false,
    migrations: [CreateAudit1700000000000],
    entities: [],
    logging: ["error"],
  });
  await ds.initialize();
  const applied = await ds.runMigrations();
  assert.equal(applied.length, 1);
  await ds.query("INSERT INTO audit (action) VALUES ('created')");
  const n = await ds.query("SELECT count(*)::int AS n FROM audit");
  assert.equal(n[0].n, 1);
  // Re-running applies nothing new.
  const again = await ds.runMigrations();
  assert.equal(again.length, 0);
  await ds.destroy();
});
