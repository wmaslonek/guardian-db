# GuardianDB + TypeORM example

A TypeORM application that talks to GuardianDB **as if it were PostgreSQL** —
using the standard `type: "postgres"` driver, with no GuardianDB-specific code.

It demonstrates: `DataSource` initialization, a migration authored with
`QueryRunner`, entities with relations (`Org` → `User` → `Post`), repository
saves/finds/updates/deletes, `findOneBy`, eager relation loading, QueryBuilder
joins and aggregates, a transaction, generated integer **and** UUID ids, JSONB
columns, timestamp columns, unique constraints and indexes.

## Run it

```bash
# 1. Build the gateway (from the repo root)
cargo build -p guardian-pgwire

# 2. Run the self-contained demo (spawns the gateway, migrates, seeds, queries)
cd examples/postgres-typeorm
npm install
npm run demo
```

Expected output (abridged):

```
gateway ready on 127.0.0.1:NNNNN
DataSource initialized
migrations applied: Init1700000000000
seeded: 2 orgs, 2 users, 3 posts
findOneBy: Alice settings= {"theme":"dark"}
relations: org = Acme posts = 2
post counts by author: [{"author":"Alice","posts":2},{"author":"Bob","posts":1}]
published titles: Globex update, Hello GuardianDB
transaction: reassigned + published a post
updated bob -> Robert: Robert
deleted unpublished posts: 3 -> 2
Demo complete ✅
```

## Connecting to a long-running gateway

In a real deployment, start the gateway separately and point TypeORM at it:

```bash
cargo run -p guardian-pgwire            # listens on 127.0.0.1:15432
```

```ts
const ds = new DataSource({
  type: "postgres",
  host: "127.0.0.1",
  port: 15432,
  username: "guardian",
  password: "guardian",
  database: "app",
  synchronize: true,
  entities: [User, Post, Org],
});
```

The TypeORM migration CLI works too:

```bash
PGPORT=15432 npm run migration:run
```

## Native GuardianDB driver (optional)

The `@guardiandb/typeorm` package (`packages/guardian-typeorm`) offers a
`GuardianDataSource` convenience that manages an embedded gateway for you. See
its README. The PostgreSQL wire path shown here is the primary, required path.
