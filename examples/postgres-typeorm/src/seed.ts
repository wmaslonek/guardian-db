import { DataSource } from "typeorm";
import { Org } from "./entities/Org";
import { User } from "./entities/User";
import { Post } from "./entities/Post";

/** Insert a small amount of sample data using the Repository API. */
export async function seed(ds: DataSource): Promise<void> {
  const orgs = ds.getRepository(Org);
  const users = ds.getRepository(User);
  const posts = ds.getRepository(Post);

  const acme = await orgs.save(orgs.create({ name: "Acme" }));
  const globex = await orgs.save(orgs.create({ name: "Globex" }));

  const alice = await users.save(
    users.create({ email: "alice@example.com", name: "Alice", org: acme, settings: { theme: "dark" } }),
  );
  const bob = await users.save(
    users.create({ email: "bob@example.com", name: "Bob", org: globex, settings: { theme: "light" } }),
  );

  await posts.save([
    posts.create({ title: "Hello GuardianDB", body: "First post", author: alice, published: true, meta: { tags: ["intro"] } }),
    posts.create({ title: "Postgres on P2P", body: "Wire-compatible", author: alice, published: false, meta: { tags: ["sql", "p2p"] } }),
    posts.create({ title: "Globex update", body: null, author: bob, published: true, meta: { tags: [] } }),
  ]);

  console.log("seeded: 2 orgs, 2 users, 3 posts");
}
