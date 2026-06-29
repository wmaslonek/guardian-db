// Conformance tests using the standard `pg` (node-postgres) client — the driver
// TypeORM, Prisma's pg adapter, and most Node tooling sit on top of.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import pg from "pg";
import { startGateway, Gateway } from "./harness";

let gw: Gateway;

function client(): pg.Client {
  return new pg.Client({
    host: "127.0.0.1",
    port: gw.port,
    user: "guardian",
    password: "guardian",
    database: "app",
  });
}

before(async () => {
  gw = await startGateway();
});
after(() => gw?.stop());

test("connect, simple query, command tags", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE t (id INT PRIMARY KEY, name TEXT NOT NULL)");
  const ins = await c.query("INSERT INTO t VALUES (1,'a'),(2,'b'),(3,'c')");
  assert.equal(ins.rowCount, 3);
  const sel = await c.query("SELECT id, name FROM t ORDER BY id");
  assert.equal(sel.rows.length, 3);
  assert.equal(sel.rows[0].name, "a");
  await c.end();
});

test("parameterized (extended) queries and prepared reuse", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE p (id INT PRIMARY KEY, v INT)");
  await c.query("INSERT INTO p VALUES ($1,$2)", [1, 10]);
  await c.query("INSERT INTO p VALUES ($1,$2)", [2, 20]);
  const r = await c.query({
    name: "by-id",
    text: "SELECT v FROM p WHERE id = $1",
    values: [2],
  });
  assert.equal(r.rows[0].v, 20);
  // Reuse the named prepared statement with new params.
  const r2 = await c.query({ name: "by-id", text: "SELECT v FROM p WHERE id = $1", values: [1] });
  assert.equal(r2.rows[0].v, 10);
  await c.end();
});

test("type round-trips (int, text, bool, numeric, jsonb, timestamp, uuid)", async () => {
  const c = client();
  await c.connect();
  await c.query(
    `CREATE TABLE types_t (
       id SERIAL PRIMARY KEY, b BOOLEAN, n NUMERIC(12,2),
       j JSONB, ts TIMESTAMPTZ, u UUID, txt TEXT)`,
  );
  await c.query(
    "INSERT INTO types_t (b,n,j,ts,u,txt) VALUES ($1,$2,$3,$4,$5,$6)",
    [true, "123.45", JSON.stringify({ a: [1, 2] }), "2026-06-29T10:00:00Z",
     "00000000-0000-0000-0000-000000000009", "héllo"],
  );
  const r = await c.query("SELECT id, b, n, j, u, txt FROM types_t");
  const row = r.rows[0];
  assert.equal(row.id, 1);
  assert.equal(row.b, true);
  assert.equal(row.n, "123.45");
  assert.deepEqual(row.j, { a: [1, 2] });
  assert.equal(row.u, "00000000-0000-0000-0000-000000000009");
  assert.equal(row.txt, "héllo");
  // Field type OIDs are PostgreSQL's.
  const f = Object.fromEntries(r.fields.map((x) => [x.name, x.dataTypeID]));
  assert.equal(f.id, 23); // int4
  assert.equal(f.j, 3802); // jsonb
  assert.equal(f.u, 2950); // uuid
  await c.end();
});

test("RETURNING and ON CONFLICT", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE kv (k TEXT PRIMARY KEY, v INT)");
  const ins = await c.query("INSERT INTO kv VALUES ('a',1) RETURNING k, v");
  assert.equal(ins.rows[0].v, 1);
  await c.query("INSERT INTO kv VALUES ('a',9) ON CONFLICT (k) DO UPDATE SET v = excluded.v");
  const r = await c.query("SELECT v FROM kv WHERE k='a'");
  assert.equal(r.rows[0].v, 9);
  await c.end();
});

test("SQLSTATE error codes propagate", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE e (id INT PRIMARY KEY)");
  await c.query("INSERT INTO e VALUES (1)");
  await assert.rejects(() => c.query("SELECT * FROM missing_table"), (e: any) => e.code === "42P01");
  await assert.rejects(() => c.query("INSERT INTO e VALUES (1)"), (e: any) => e.code === "23505");
  await assert.rejects(() => c.query("INSERT INTO e VALUES (NULL)"), (e: any) => e.code === "23502");
  await c.end();
});

test("transactions: commit and rollback", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE tx (id INT PRIMARY KEY)");
  await c.query("BEGIN");
  await c.query("INSERT INTO tx VALUES (1)");
  await c.query("ROLLBACK");
  assert.equal((await c.query("SELECT count(*)::int n FROM tx")).rows[0].n, 0);
  await c.query("BEGIN");
  await c.query("INSERT INTO tx VALUES (1)");
  await c.query("COMMIT");
  assert.equal((await c.query("SELECT count(*)::int n FROM tx")).rows[0].n, 1);
  await c.end();
});

test("concurrent connections share data", async () => {
  const a = client();
  const b = client();
  await a.connect();
  await b.connect();
  await a.query("CREATE TABLE shared (id INT PRIMARY KEY)");
  await a.query("INSERT INTO shared VALUES (1),(2)");
  const r = await b.query("SELECT count(*)::int n FROM shared");
  assert.equal(r.rows[0].n, 2);
  await a.end();
  await b.end();
});

test("aggregates, joins and grouping over the wire", async () => {
  const c = client();
  await c.connect();
  await c.query("CREATE TABLE dept (id INT PRIMARY KEY, name TEXT)");
  await c.query("CREATE TABLE emp (id INT PRIMARY KEY, dept_id INT, salary INT)");
  await c.query("INSERT INTO dept VALUES (1,'eng'),(2,'sales')");
  await c.query("INSERT INTO emp VALUES (1,1,100),(2,1,120),(3,2,90)");
  const r = await c.query(
    `SELECT d.name, count(*)::int AS headcount, sum(e.salary)::int AS total
     FROM emp e JOIN dept d ON e.dept_id = d.id
     GROUP BY d.name HAVING sum(e.salary) > 100 ORDER BY d.name`,
  );
  assert.equal(r.rows.length, 1);
  assert.equal(r.rows[0].name, "eng");
  assert.equal(r.rows[0].headcount, 2);
  assert.equal(r.rows[0].total, 220);
  await c.end();
});
