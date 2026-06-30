# GuardianDB TypeScript ODM

This directory contains the optional TypeScript/JavaScript ODM proposed in issue #17. It is transport-driven: the high-level `GuardianDB` and `Collection<T>` APIs do not depend on one particular Node, browser, React Native, or WASM binding.

```ts
import GuardianDB from "@guardiandb/odm";
import Iroh from "iroh";

const iroh = await Iroh.create();
const db = await GuardianDB.init("DatabaseName", iroh, { path: "./.guardiandb" });
const employees = await db.initCollection("employees");

await employees.insertOne({ name: "Elon", ssn: "562-48-5384", hourly_pay: "$15" });
const employee = await employees.findOne({ ssn: "562-48-5384" });
const updated = await employees.update(
  { ssn: "562-48-5384" },
  { $set: { hourly_pay: "$100" } },
);

console.log(await GuardianDB.listDatabases());
console.log(await db.listCollections());
```

Typed schemas add validation, primary keys, unique and secondary indexes, defaults, custom validators, strict mode, and timestamps:

```ts
import { defineSchema, type Document } from "@guardiandb/odm";

interface Employee extends Document {
  id?: string;
  ssn: string;
  name: string;
}

const schema = defineSchema<Employee>({
  timestamps: true,
  fields: {
    id: { type: String, primaryKey: true },
    ssn: { type: String, required: true, unique: true },
    name: { type: String, required: true, index: true },
  },
});

const employees = await db.initCollection<Employee>("employees", { schema });
```

## Transport integration

A native GuardianDB/Iroh binding should expose a `GuardianTransport` as `iroh.guardianDBTransport`, or callers can pass it explicitly in `GuardianDB.init(..., { transport })`. The included `MemoryTransport` is a deterministic process-local reference implementation used by tests and development; it does not provide decentralized persistence. Until a native adapter is supplied, `GuardianDB.init` falls back to that reference transport so the SDK surface remains executable.

Writes with the default `local_atomic` transaction context serialize validation, index maintenance, and mutation per collection. The `replicated` consistency value is reserved for a future distributed coordinator and is rejected by the reference transport rather than implying cross-peer ACID guarantees.
