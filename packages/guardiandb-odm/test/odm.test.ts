import assert from "node:assert/strict";
import test from "node:test";

import GuardianDB, {
  DuplicateKeyError,
  MemoryTransport,
  ValidationError,
  defineSchema,
  type Document,
} from "../src/index.js";

interface Employee extends Document {
  employeeId?: string;
  name: string;
  ssn: string;
  email?: string | null;
  skills?: string[];
  hourly_pay: string;
  createdAt?: string;
  updatedAt?: string;
}

const employeeSchema = defineSchema<Employee>({
  strict: true,
  timestamps: true,
  fields: {
    employeeId: { type: String, primaryKey: true },
    name: { type: String, required: true },
    ssn: { type: String, required: true, unique: true },
    email: { type: String, unique: true, nullable: true },
    skills: { type: Array, index: true },
    hourly_pay: { type: String, required: true },
    createdAt: { type: "timestamp" },
    updatedAt: { type: "timestamp" },
  },
});

test.beforeEach(() => {
  MemoryTransport.shared.reset();
});

test("target Mongoose-style usage works", async () => {
  const guardiandb = await GuardianDB.init("DatabaseName", {}, { path: "./.guardiandb" });
  const collection = await guardiandb.initCollection("employees");

  const inserted = await collection.insertOne({
    name: "Elon",
    ssn: "562-48-5384",
    hourly_pay: "$15",
  });
  assert.equal(typeof inserted._id, "string");

  const employee = await collection.findOne({ ssn: "562-48-5384" });
  assert.equal(employee?.name, "Elon");

  const updatedEmployee = await collection.update(
    { ssn: "562-48-5384" },
    { $set: { hourly_pay: "$100" } },
  );
  assert.equal(updatedEmployee?.hourly_pay, "$100");
  assert.deepEqual(await guardiandb.listCollections(), ["employees"]);
  assert.deepEqual(await GuardianDB.listDatabases(), ["DatabaseName"]);
});

test("typed schemas validate required fields and enforce unique indexes", async () => {
  const db = await GuardianDB.init("constraints", {});
  const employees = await db.initCollection<Employee>("employees", { schema: employeeSchema });

  const first = await employees.insertOne({
    name: "Ada",
    ssn: "111-22-3333",
    email: "ada@example.test",
    skills: ["math", "compilers"],
    hourly_pay: "$50",
  });
  assert.equal(typeof first.employeeId, "string");
  assert.equal(typeof first.createdAt, "string");
  assert.equal(typeof first.updatedAt, "string");
  assert.equal((await employees.find({ skills: "math" })).length, 1);

  await employees.insertOne({
    name: "Null Email",
    ssn: "222-33-4444",
    email: null,
    hourly_pay: "$40",
  });
  assert.equal((await employees.find({ email: null })).length, 1);

  await assert.rejects(
    employees.insertOne({
      name: "Grace",
      ssn: "111-22-3333",
      hourly_pay: "$60",
    }),
    DuplicateKeyError,
  );

  await assert.rejects(
    employees.insertOne({
      name: "Missing Pay",
      ssn: "999-99-9999",
    } as Employee),
    ValidationError,
  );
});

test("multi-insert validates atomically before committing", async () => {
  const db = await GuardianDB.init("batch", {});
  const employees = await db.initCollection<Employee>("employees", { schema: employeeSchema });

  await assert.rejects(
    employees.insert([
      { name: "One", ssn: "same", hourly_pay: "$1" },
      { name: "Two", ssn: "same", hourly_pay: "$2" },
    ]),
    DuplicateKeyError,
  );
  assert.deepEqual(await employees.find({}), []);
});

test("primary keys are queryable and immutable", async () => {
  const db = await GuardianDB.init("ids", {});
  const employees = await db.initCollection<Employee>("employees", { schema: employeeSchema });
  const inserted = await employees.insertOne({
    employeeId: "employee-1",
    name: "Lin",
    ssn: "123",
    hourly_pay: "$10",
  });

  assert.equal((await employees.findById("employee-1"))?.name, "Lin");
  assert.equal((await employees.find({ hourly_pay: { $in: ["$10", "$20"] } })).length, 1);

  await assert.rejects(
    employees.update(
      { employeeId: inserted.employeeId! },
      { $set: { employeeId: "employee-2" } },
    ),
    (error: unknown) =>
      error instanceof Error && error.message.includes("Immutable field 'employeeId'"),
  );
});

test("$inc, $unset, dot paths, and secondary indexes work", async () => {
  interface Counter extends Document {
    id: string;
    group: string;
    stats: { count: number; obsolete?: boolean };
  }

  const db = await GuardianDB.init("updates", {});
  const counters = await db.initCollection<Counter>("counters", {
    schema: {
      fields: {
        id: { type: String, primaryKey: true },
        group: { type: String, index: true },
        stats: { type: Object, required: true },
      },
    },
  });

  await counters.insertOne({ id: "one", group: "a", stats: { count: 1, obsolete: true } });
  const updated = await counters.update(
    { group: "a" },
    { $inc: { "stats.count": 2 }, $unset: { "stats.obsolete": true } },
  );
  assert.deepEqual(updated?.stats, { count: 3 });
});

test("failed unique updates leave documents and indexes unchanged", async () => {
  const db = await GuardianDB.init("update-rollback", {});
  const employees = await db.initCollection<Employee>("employees", { schema: employeeSchema });

  await employees.insert([
    {
      employeeId: "employee-a",
      name: "Ada",
      ssn: "ssn-a",
      skills: ["math"],
      hourly_pay: "$50",
    },
    {
      employeeId: "employee-b",
      name: "Grace",
      ssn: "ssn-b",
      skills: ["compilers"],
      hourly_pay: "$60",
    },
  ]);

  await assert.rejects(
    employees.update(
      { employeeId: "employee-a" },
      { $set: { ssn: "ssn-b", skills: ["distributed"] } },
    ),
    DuplicateKeyError,
  );

  assert.equal((await employees.findOne({ ssn: "ssn-a" }))?.employeeId, "employee-a");
  assert.equal(await employees.findOne({ ssn: "ssn-b" }).then((employee) => employee?.employeeId), "employee-b");
  assert.equal((await employees.find({ skills: "math" })).length, 1);
  assert.equal((await employees.find({ skills: "distributed" })).length, 0);

  const updated = await employees.update(
    { employeeId: "employee-a" },
    { $set: { ssn: "ssn-c", skills: ["distributed"] } },
  );
  assert.equal(updated?.ssn, "ssn-c");
  assert.equal(await employees.findOne({ ssn: "ssn-a" }), null);
  assert.equal((await employees.findOne({ ssn: "ssn-c" }))?.employeeId, "employee-a");
  assert.equal((await employees.find({ skills: "math" })).length, 0);
  assert.equal((await employees.find({ skills: "distributed" })).length, 1);
});

test("repeated indexed updates do not rebuild the entire collection", { timeout: 5_000 }, async () => {
  interface IndexedCounter extends Document {
    id: string;
    email: string;
    group: string;
    counter: number;
  }

  const db = await GuardianDB.init("incremental-indexes", {});
  const counters = await db.initCollection<IndexedCounter>("counters", {
    schema: {
      fields: {
        id: { type: String, primaryKey: true },
        email: { type: String, required: true, unique: true },
        group: { type: String, required: true, index: true },
        counter: { type: Number, required: true },
      },
    },
  });

  const documents = Array.from({ length: 5_000 }, (_, index) => ({
    id: `counter-${index}`,
    email: `counter-${index}@example.test`,
    group: `group-${index % 32}`,
    counter: index,
  }));
  await counters.insert(documents);

  for (let index = 0; index < 500; index += 1) {
    const updated = await counters.update(
      { email: `counter-${index}@example.test` },
      { $set: { group: "hot" }, $inc: { counter: 1 } },
    );
    assert.equal(updated?.group, "hot");
  }

  assert.equal((await counters.find({ group: "hot" })).length, 500);
  assert.equal((await counters.find({ group: "group-0" })).length, 141);
});

test("replicated transaction contexts are reserved instead of overpromising ACID", async () => {
  const db = await GuardianDB.init("transactions", {});
  const employees = await db.initCollection<Employee>("employees", { schema: employeeSchema });
  const transaction = employees.beginTransaction("replicated");

  await assert.rejects(
    employees.insertOne(
      { name: "Reserved", ssn: "tx", hourly_pay: "$1" },
      { transaction },
    ),
    (error: unknown) =>
      error instanceof Error && error.message.includes("distributed coordinator"),
  );
});
