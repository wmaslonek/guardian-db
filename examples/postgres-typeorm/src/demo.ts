// Self-contained runnable demo: spawns the GuardianDB PostgreSQL gateway, runs
// the TypeORM migration, seeds data, and exercises the query/transaction
// examples — then tears the gateway down. Run with `npm run demo`.
//
// In a real deployment you would start the gateway separately
// (`cargo run -p guardian-pgwire`) and just point TypeORM at it.

import "reflect-metadata";
import { spawn, ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import fs from "node:fs";
import { DataSource } from "typeorm";
import { options } from "./data-source";
import { seed } from "./seed";
import { runQueries } from "./queries";

function gatewayBinary(): string {
  if (process.env.GUARDIAN_PGWIRE_BIN) return process.env.GUARDIAN_PGWIRE_BIN;
  const root = path.resolve(__dirname, "..", "..", "..");
  for (const rel of ["target/debug/guardian-pgwire", "target/release/guardian-pgwire"]) {
    const full = path.join(root, rel);
    if (fs.existsSync(full)) return full;
  }
  throw new Error("Build the gateway first: cargo build -p guardian-pgwire");
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

async function waitPort(port: number): Promise<void> {
  for (let i = 0; i < 80; i++) {
    const ok = await new Promise<boolean>((res) => {
      const s = net.connect(port, "127.0.0.1");
      s.on("connect", () => { s.destroy(); res(true); });
      s.on("error", () => res(false));
    });
    if (ok) return;
    await new Promise((r) => setTimeout(r, 120));
  }
  throw new Error("gateway did not start");
}

async function main() {
  const port = await freePort();
  const proc: ChildProcess = spawn(gatewayBinary(), ["--addr", `127.0.0.1:${port}`, "--database", "app"], {
    stdio: "ignore",
  });
  try {
    await waitPort(port);
    console.log(`gateway ready on 127.0.0.1:${port}`);

    const ds = new DataSource(options({ port }));
    await ds.initialize();
    console.log("DataSource initialized");

    const applied = await ds.runMigrations();
    console.log("migrations applied:", applied.map((m) => m.name).join(", ") || "(none)");

    await seed(ds);
    await runQueries(ds);

    await ds.destroy();
    console.log("\nDemo complete ✅");
  } finally {
    proc.kill("SIGKILL");
  }
}

main().catch((e) => {
  console.error("Demo failed:", e);
  process.exitCode = 1;
});
