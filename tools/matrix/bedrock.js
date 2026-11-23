// A headless Bedrock client (RakNet) joins through Geyser -> Vine -> Pumpkin,
// chats, and switches servers twice with Vine's /server command.
// Usage: node bedrock.js [host] [port] [gamertag] [xuid]
// The xuid is what Geyser puts in Floodgate's handoff (Geyser here doesn't
// validate Bedrock logins), so Vine gives the bot UUID 0-0-xuid.
const bedrock = require('bedrock-protocol');

const host = process.argv[2] || '127.0.0.1';
const port = Number(process.argv[3] || 19132);
const username = process.argv[4] || 'BedrockBot';
const xuid = process.argv[5] || '2535400000000001';
const r = { username, xuid, join: false, chunks: 0, chat: false, switches: [], messages: [], errors: [], end: null };
const sleep = (ms) => new Promise((res) => setTimeout(res, ms));

const client = bedrock.createClient({ host, port, username, offline: true, skipPing: false, connectTimeout: 15000 });
// Offline sessions send xuid 0; set ours once the profile exists, before the
// login chain is built.
client.once('connect_allowed', () => { if (client.profile) client.profile.xuid = xuid; });
let spawns = 0;
let dimensionChanges = 0;
let runtimeId = 0n;

client.on('start_game', (p) => { runtimeId = p.runtime_entity_id; });
client.on('spawn', () => { spawns++; });
client.on('level_chunk', () => { r.chunks++; });
client.on('change_dimension', () => { dimensionChanges++; });
client.on('text', (p) => {
  const m = String(p.message || '') + (p.parameters && p.parameters.length ? ' ' + p.parameters.join(' ') : '');
  if (r.messages.length < 14) r.messages.push(m.slice(0, 140));
  if (m.includes('hello-from-bedrock')) r.chat = true;
});
client.on('error', (e) => r.errors.push(String(e && e.message || e).slice(0, 200)));
client.on('kick', (p) => { r.end = 'kicked: ' + JSON.stringify(p).slice(0, 200); });
client.on('close', () => { if (!r.end) r.end = 'closed'; });

async function waitFor(pred, ms) {
  const until = Date.now() + ms;
  while (Date.now() < until) { if (pred()) return true; await sleep(100); }
  return false;
}

function chat(message) {
  client.queue('text', { category: 'authored', type: 'chat', needs_translation: false, source_name: client.username, xuid: '', platform_chat_id: '', filtered_message: '', message, has_filtered_message: false });
}
function command(cmd) {
  client.queue('command_request', { command: cmd, origin: { type: 'player', uuid: client.profile ? client.profile.uuid : '00000000-0000-0000-0000-000000000000', request_id: '', player_entity_id: runtimeId }, internal: false, version: 'latest' });
}

async function switchTo(server) {
  const before = { dims: dimensionChanges, chunks: r.chunks };
  command('/server ' + server);
  const ok = await waitFor(() => dimensionChanges > before.dims, 15000);
  await sleep(2500);
  r.switches.push({ server, ok: ok && !r.end, chunksAfter: r.chunks - before.chunks });
}

let done = false;
function finish() {
  if (done) return; done = true;
  r.stillConnected = !r.end;
  console.log(JSON.stringify(r));
  try { client.close(); } catch (_) {}
  setTimeout(() => process.exit(0), 500);
}

async function main() {
  r.join = await waitFor(() => spawns > 0, 45000);
  if (!r.join) return finish();
  await sleep(3000);
  chat('hello-from-bedrock');
  await waitFor(() => r.chat, 4000);
  await switchTo('game-1');
  if (!r.end) await switchTo('lobby');
  finish();
}
setTimeout(finish, 90000);
main().catch((e) => { r.errors.push('main: ' + e.message); finish(); });
