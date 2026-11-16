// 1.7.10 isn't supported by mineflayer, so this uses minecraft-protocol directly:
// join, chat, then /server game-1 and back to lobby. Usage: node java-1.7.js [host] [port]
const mc = require('minecraft-protocol');
const host = process.argv[2] || '127.0.0.1';
const port = Number(process.argv[3] || 25565);
const r = { version: '1.7.10', login: false, chunks: 0, chat: false, packets: {}, errors: [], end: null };
const c = mc.createClient({ host, port, username: 'mx1_7_10', version: '1.7', auth: 'offline', hideErrors: true });
c.on('packet', (d, m) => { r.packets[m.name] = (r.packets[m.name] || 0) + 1; });
c.on('login', () => { if (r.login) { r.joins++; return; } r.login = true; r.joins = 1; setTimeout(() => c.write('chat', { message: 'hello-from-1.7.10' }), 2000); setTimeout(() => c.write('chat', { message: '/server game-1' }), 4000); setTimeout(() => c.write('chat', { message: '/server lobby' }), 8000); });
c.on('map_chunk_bulk', () => r.chunks++); c.on('map_chunk', () => r.chunks++);
c.on('chat', (p) => { if (String(p.message).includes('hello-from')) r.chat = true; });
c.on('error', (e) => r.errors.push(String(e.message).slice(0, 200)));
c.on('end', (reason) => { r.end = 'end: ' + reason; });
setTimeout(() => { console.log(JSON.stringify(r)); process.exit(0); }, 14000);
