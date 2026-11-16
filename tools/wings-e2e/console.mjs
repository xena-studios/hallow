// A server console as the panel's browser client uses it: the websocket the
// panel hands out (`{token, url}`), Wings' event protocol over it. Used by
// run.sh; needs Node.js 22 or newer (the built-in WebSocket and fetch).
//
//   node console.mjs '<the panel's {"token", "url"} JSON>' [step...]
//
// Steps run in order, each waiting at most TIMEOUT_SECS (default 60):
//
//   log=TEXT        console output (history from "send logs", or live) contains TEXT
//   command=TEXT    sends TEXT as a console command over the socket
//   wait=SECS       waits
//   status=STATE    a "status" event with STATE (running, offline, starting,
//                   stopping) after the last http step or status matched
//   http=METHOD,URL,BODY
//                   calls the panel's API (Authorization from PANEL_API_KEY),
//                   such as a power action
//
// Prints every console line and status change, and exits 1 with the reason on
// a timeout, a closed socket or a refused token.

const [answer, ...steps] = process.argv.slice(2);
const { token, url } = JSON.parse(answer ?? '{}');
if (!token || !url) {
  console.error('console.mjs: the first argument must be the panel\'s {"token", "url"} answer');
  process.exit(2);
}
const timeoutMs = Number(process.env.TIMEOUT_SECS ?? 60) * 1000;

const lines = [];
const statuses = [];
let cursor = 0;
let waiter = null;
let failure = null;

function fail(reason) {
  failure ??= reason;
  waiter?.();
}

function check() {
  waiter?.();
}

// Resolves once `test()` is true; checked on every event.
function until(what, test) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), timeoutMs);
    waiter = () => {
      if (failure) {
        clearTimeout(timer);
        reject(new Error(failure));
      } else if (test()) {
        clearTimeout(timer);
        waiter = null;
        resolve();
      }
    };
    waiter();
  });
}

const ws = new WebSocket(url);
const send = (event, ...args) => ws.send(JSON.stringify({ event, args }));
let authed = false;

ws.addEventListener('error', (e) => fail(`websocket error: ${e.message ?? e.type}`));
ws.addEventListener('close', (e) => fail(`websocket closed (${e.code} ${e.reason})`));
ws.addEventListener('open', () => send('auth', token));
ws.addEventListener('message', async (e) => {
  const text = typeof e.data === 'string' ? e.data : Buffer.from(await e.data.arrayBuffer()).toString();
  let message;
  try {
    message = JSON.parse(text);
  } catch {
    return;
  }
  const args = message.args ?? [];
  switch (message.event) {
    case 'auth success':
      authed = true;
      console.log('  ws  auth success');
      send('send logs');
      break;
    case 'jwt error':
    case 'token expired':
      fail(`${message.event}: ${args.join(' ')}`);
      break;
    case 'console output':
    case 'daemon message':
      for (const line of args.join('\n').split('\n')) {
        const plain = line.replace(/\x1b\[[0-9;]*[A-Za-z]/g, '');
        lines.push(plain);
        console.log(`  ws  | ${plain}`);
      }
      break;
    case 'status':
      statuses.push(args[0]);
      console.log(`  ws  status ${args[0]}`);
      break;
    case 'daemon error':
      console.log(`  ws  daemon error: ${args.join(' ')}`);
      break;
  }
  check();
});

try {
  await until('auth success', () => authed);
  // Wings checks a socket's permissions once a second before it subscribes it
  // to live output (routes' websocket handler), so output in the first second
  // after "auth success" only reaches the socket through "send logs".
  await new Promise((resolve) => setTimeout(resolve, 1500));
  for (const step of steps) {
    const [kind, ...rest] = step.split('=');
    const value = rest.join('=');
    if (kind === 'log') {
      await until(`console output containing "${value}"`, () => lines.some((l) => l.includes(value)));
    } else if (kind === 'wait') {
      await new Promise((resolve) => setTimeout(resolve, Number(value) * 1000));
    } else if (kind === 'command') {
      send('send command', value);
      console.log(`  ws  > ${value}`);
    } else if (kind === 'status') {
      // Status events since the last http step, or the last status matched.
      await until(`status ${value}`, () => statuses.indexOf(value, cursor) >= 0);
      cursor = statuses.indexOf(value, cursor) + 1;
    } else if (kind === 'http') {
      cursor = statuses.length;
      const [method, target, ...body] = value.split(',');
      const response = await fetch(target, {
        method,
        headers: {
          Authorization: `Bearer ${process.env.PANEL_API_KEY}`,
          'Content-Type': 'application/json',
        },
        body: body.length ? body.join(',') : undefined,
      });
      console.log(`  ws  ${method} ${target}: ${response.status}`);
      if (!response.ok) throw new Error(`${method} ${target} answered ${response.status}: ${await response.text()}`);
    } else {
      throw new Error(`unknown step ${step}`);
    }
  }
} catch (err) {
  console.error(`console.mjs: ${err.message}`);
  process.exit(1);
}
ws.close();
process.exit(0);
