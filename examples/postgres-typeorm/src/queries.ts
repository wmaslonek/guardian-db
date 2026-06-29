import { DataSource } from "typeorm";
import { User } from "./entities/User";
import { Post } from "./entities/Post";

/** Demonstrate the common TypeORM read/write surfaces. */
export async function runQueries(ds: DataSource): Promise<void> {
  const users = ds.getRepository(User);
  const posts = ds.getRepository(Post);

  // findOneBy
  const alice = await users.findOneBy({ email: "alice@example.com" });
  console.log("findOneBy:", alice?.name, "settings=", JSON.stringify(alice?.settings));

  // find with relations
  const withRelations = await users.findOne({
    where: { email: "alice@example.com" },
    relations: { org: true, posts: true },
  });
  console.log("relations: org =", withRelations?.org?.name, "posts =", withRelations?.posts.length);

  // QueryBuilder join + aggregate
  const counts = await posts
    .createQueryBuilder("p")
    .innerJoin("p.author", "u")
    .select("u.name", "author")
    .addSelect("COUNT(*)::int", "posts")
    .groupBy("u.name")
    .orderBy("u.name", "ASC")
    .getRawMany();
  console.log("post counts by author:", JSON.stringify(counts));

  // QueryBuilder with where + order
  const published = await posts
    .createQueryBuilder("p")
    .where("p.published = :pub", { pub: true })
    .orderBy("p.title", "ASC")
    .getMany();
  console.log("published titles:", published.map((p) => p.title).join(", "));

  // Transaction: reassign a post to Bob atomically (it stays unpublished).
  await ds.transaction(async (m) => {
    const bob = await m.getRepository(User).findOneByOrFail({ email: "bob@example.com" });
    const post = await m.getRepository(Post).findOneByOrFail({ title: "Postgres on P2P" });
    post.author = bob;
    await m.getRepository(Post).save(post);
  });
  console.log("transaction: reassigned a post to Bob");

  // Update + delete
  await users.update({ email: "bob@example.com" }, { name: "Robert" });
  console.log("updated bob -> Robert:", (await users.findOneBy({ email: "bob@example.com" }))?.name);

  const before = await posts.count();
  await posts.delete({ published: false });
  console.log(`deleted unpublished posts: ${before} -> ${await posts.count()}`);
}
