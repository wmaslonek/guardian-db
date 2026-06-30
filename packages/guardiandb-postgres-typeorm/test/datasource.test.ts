import "reflect-metadata";
import { test } from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { Entity, PrimaryGeneratedColumn, Column } from "typeorm";
import { GuardianDataSource } from "../src/index";

@Entity()
class Item {
  @PrimaryGeneratedColumn() id!: number;
  @Column({ type: "text" }) label!: string;
  @Column({ type: "jsonb", default: {} }) attrs!: any;
}

function freePort(): Promise<number> {
  return new Promise((res, rej) => {
    const s = net.createServer();
    s.on("error", rej);
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as net.AddressInfo).port;
      s.close(() => res(p));
    });
  });
}

test("GuardianDataSource manages an embedded gateway and runs TypeORM", async () => {
  const port = await freePort();
  const ds = new GuardianDataSource({
    path: "./data",
    database: "app",
    peers: [],
    consistency: "strict",
    port,
    entities: [Item],
    synchronize: true,
  });
  await ds.initialize();
  try {
    const repo = ds.getRepository(Item);
    const saved = await repo.save(repo.create({ label: "widget", attrs: { color: "red" } }));
    assert.equal(typeof saved.id, "number");
    const found = await repo.findOneBy({ label: "widget" });
    assert.equal(found?.label, "widget");
    assert.deepEqual(found?.attrs, { color: "red" });
    assert.equal(await repo.count(), 1);
  } finally {
    await ds.destroy();
  }
});
