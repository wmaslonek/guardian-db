// Shared test harness: locates and spawns the `guardian-pgwire` gateway on a
// free port, waits for it to accept connections, and tears it down.

import { spawn, ChildProcess } from "node:child_process";
import net from "node:net";
import path from "node:path";
import fs from "node:fs";

export interface Gateway {
  port: number;
  stop(): void;
}

function binaryPath(): string {
  if (process.env.GUARDIAN_PGWIRE_BIN) return process.env.GUARDIAN_PGWIRE_BIN;
  const root = path.resolve(__dirname, "..", "..");
  for (const rel of ["target/debug/guardian-pgwire", "target/release/guardian-pgwire"]) {
    const full = path.join(root, rel);
    if (fs.existsSync(full)) return full;
  }
  throw new Error(
    "guardian-pgwire binary not found. Build it first: `cargo build --features pgwire --bin guardian-pgwire`",
  );
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.unref();
    s.on("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(port));
    });
  });
}

function probe(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const s = net.connect(port, "127.0.0.1");
    s.on("connect", () => {
      s.destroy();
      resolve(true);
    });
    s.on("error", () => resolve(false));
  });
}

async function waitPort(port: number, timeoutMs = 10_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (await probe(port)) return;
    await new Promise((r) => setTimeout(r, 120));
  }
  throw new Error(`gateway did not start on port ${port}`);
}

/** Start a fresh in-memory GuardianDB PostgreSQL gateway. */
export async function startGateway(database = "app"): Promise<Gateway> {
  const port = await freePort();
  const proc: ChildProcess = spawn(
    binaryPath(),
    ["--addr", `127.0.0.1:${port}`, "--database", database],
    { stdio: "ignore" },
  );
  proc.on("error", (e) => {
    throw e;
  });
  await waitPort(port);
  return { port, stop: () => proc.kill("SIGKILL") };
}
