import { createRelayServer } from "./relay.js";

const port = Number.parseInt(process.env.PORT ?? "18080", 10);
const host = process.env.HOST ?? "0.0.0.0";

if (!Number.isInteger(port) || port < 1 || port > 65_535) {
  throw new Error(`Invalid PORT: ${process.env.PORT}`);
}

const relay = createRelayServer({ host, port });

async function shutdown(signal) {
  console.info(`Received ${signal}; shutting down`);
  await relay.stop();
  process.exit(0);
}

process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));

await relay.start();
