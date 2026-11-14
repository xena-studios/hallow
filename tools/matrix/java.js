// Version matrix: one headless client per version joins through Vine, checks
// chunks, chat, inventory and tab completion, and switches servers twice.
// Usage: node java.js <version> [host] [port]
const mineflayer = require('mineflayer');

const version = process.argv[2];
const host = process.argv[3] || '127.0.0.1';
const port = Number(process.argv[4] || 25565);
const r = { version, join: false, chunks: 0, chat: false, inventory: false, tab: null, switches: [], messages: [], errors: [], end: null };

const bot = mineflayer.createBot({ host, port, version, username: 'mx' + version.replace(/\./g, '_'), auth: 'offline', hideErrors: true });
const sleep = (ms) => new Promise((res) => setTimeout(res, ms));
let spawns = 0;
let logins = 0;

bot._client.on('login', () => { logins++; });
bot.on('spawn', () => { spawns++; });
bot.on('chunkColumnLoad', () => { r.chunks++; });
bot.on('messagestr', (m) => { if (r.messages.length < 12) r.messages.push(m.slice(0, 140)); if (m.includes('hello-from-')) r.chat = true; });
bot.on('error', (e) => r.errors.push(String(e && e.message || e).slice(0, 200)));
bot.on('kicked', (reason) => { r.end = 'kicked: ' + JSON.stringify(reason).slice(0, 200); });
bot.on('end', (reason) => { if (!r.end) r.end = 'end: ' + reason; });
bot._client.on('window_items', () => { r.inventory = true; });

async function waitFor(pred, ms) {
  const until = Date.now() + ms;
  while (Date.now() < until) { if (pred()) return true; await sleep(100); }
  return false;
}

async function switchTo(server) {
  const before = { spawns, logins, respawns: respawns() };
  bot.chat('/server ' + server);
  const ok = await waitFor(() => spawns > before.spawns || logins > before.logins || respawns() > before.respawns, 15000);
  await sleep(1500);
  r.switches.push({ server, ok: ok && !r.end });
}

let respawnCount = 0;
bot._client.on('respawn', () => { respawnCount++; });
const respawns = () => respawnCount;

async function main() {
  r.join = await waitFor(() => spawns > 0, 20000);
  if (!r.join) return finish();
  await sleep(3000);
  bot.chat('hello-from-' + version);
  await waitFor(() => r.chat, 4000);
  try {
    const t = await Promise.race([bot.tabComplete('/ser'), sleep(4000).then(() => null)]);
    r.tab = t === null ? 'timeout' : JSON.stringify(t.map((x) => x.match || x)).slice(0, 120);
  } catch (e) { r.tab = 'error ' + e.message; }
  await switchTo('game-1');
  if (!r.end) await switchTo('lobby');
  if (!r.end) { bot.chat('/server nowhere'); await sleep(2000); }
  finish();
}

let done = false;
function finish() {
  if (done) return; done = true;
  r.stillConnected = !r.end;
  console.log(JSON.stringify(r));
  try { bot.quit(); } catch (_) {}
  setTimeout(() => process.exit(0), 500);
}
setTimeout(finish, 60000);
main().catch((e) => { r.errors.push('main: ' + e.message); finish(); });
