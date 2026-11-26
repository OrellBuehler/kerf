// Starts the launcher as a client would, speaks MCP to it and checks that stdout carried nothing else.
// Usage: node test/handshake.mjs   (KERF_VERSION picks the release; the first run downloads it)
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { minisignOk } from "../bin/kerf-mcp.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const check = (ok, what) => {
  if (!ok) {
    console.error(`FAIL: ${what}`);
    process.exit(1);
  }
  console.error(`ok: ${what}`);
};

check(!minisignOk("", createHash("blake2b512").digest()), "an empty signature is refused");
check(
  !minisignOk(Buffer.from(`untrusted comment: x\n${Buffer.alloc(74, 1).toString("base64")}\ntrusted comment: y\n${Buffer.alloc(64).toString("base64")}\n`).toString("base64"), createHash("blake2b512").digest()),
  "a signature by another key is refused",
);

const child = spawn(process.execPath, [join(here, "..", "bin", "kerf-mcp.mjs"), ...process.argv.slice(2)], {
  stdio: ["pipe", "pipe", "inherit"],
});
const answers = new Map();
const waiting = new Map();
createInterface({ input: child.stdout }).on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    check(false, `stdout carries only JSON-RPC, got: ${line.slice(0, 120)}`);
  }
  check(message.jsonrpc === "2.0", "a stdout line is JSON-RPC");
  if (message.id !== undefined) {
    answers.set(message.id, message);
    waiting.get(message.id)?.(message);
  }
});
const send = (message) => child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
const ask = (id, method, params) =>
  new Promise((resolve, reject) => {
    waiting.set(id, resolve);
    setTimeout(() => reject(new Error(`no answer to ${method}`)), 600_000);
    send({ id, method, params });
  });
const exited = new Promise((resolve) => child.on("exit", resolve));

const init = await ask(1, "initialize", {
  protocolVersion: "2025-06-18",
  capabilities: {},
  clientInfo: { name: "kerf-mcp-test", version: "0" },
});
check(init.result?.serverInfo?.name === "kerf", `initialize answers as kerf ${init.result?.serverInfo?.version}`);
send({ method: "notifications/initialized" });
const tools = await ask(2, "tools/list", {});
check(tools.result?.tools?.length > 10, `tools/list has ${tools.result?.tools?.length} tools`);
child.stdin.end();
const timer = setTimeout(() => check(false, "exits when stdin closes"), 60_000);
const code = await exited;
clearTimeout(timer);
check(code === 0, `exits 0 when stdin closes (got ${code})`);
process.exit(0);
