import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

const configPath = resolve("src-tauri", "tauri.conf.json");
const config = JSON.parse(await readFile(configPath, "utf8"));
const updater = config.plugins?.updater;

if (!updater || typeof updater.pubkey !== "string" || updater.pubkey.trim() === "") {
  throw new Error("src-tauri/tauri.conf.json must define plugins.updater.pubkey.");
}

const publicKey = updater.pubkey.trim();
if (Buffer.from(publicKey, "base64").toString("base64") !== publicKey) {
  throw new Error("plugins.updater.pubkey must be a Base64-encoded minisign public key.");
}

const minisignPublicKey = Buffer.from(publicKey, "base64").toString("utf8");
const keyLines = minisignPublicKey.trimEnd().split(/\r?\n/);
if (
  keyLines.length !== 2 ||
  !keyLines[0].startsWith("untrusted comment: minisign public key:") ||
  keyLines[1].trim() === ""
) {
  throw new Error(
    "plugins.updater.pubkey must decode to a two-line minisign public key; do not Base64-encode the .pub file a second time.",
  );
}

console.log("Tauri updater public key format is valid.");
