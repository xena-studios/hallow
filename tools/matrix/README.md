# Version matrix clients

Headless clients for the client table in [Validation](../../docs/VALIDATION.md#client-versions). Start the dev network first; Vine translates every version, so the servers need nothing extra:

```sh
docker compose -f deploy/compose/compose.yaml up -d --build
```

**Java 1.8.9 to 26.1** (mineflayer): join, chat, tab-complete `/ser`, walk while a second bot of the same version watches (`moved.watched` is how far the server relayed the walk), switch to `game-1` and back, try `/server nowhere`. Prints one JSON line. Usage: `node java.js <version> [host] [port]`; `java-1.7.js` takes `[host] [port]`.

```sh
cd tools/matrix && npm install --ignore-scripts
for v in 1.8.9 1.9.4 1.12.2 1.16.5 1.18.2 1.20.1 1.20.4 1.21.4 1.21.11 26.1; do node java.js $v; done
node java-1.7.js         # 1.7.10, through minecraft-protocol: join, chat, switch twice
```

26.3 is `netcheck` (`cargo run --release -p netcheck -- --route game-1,lobby`).

**Bedrock** through Geyser: start the dev network with `deploy/compose/compose.geyser.yaml` (see [deploy](../../deploy/README.md#bedrock-through-geyser)), which runs Geyser with the settings the bot needs, then:

```sh
docker build -t hallow-bedrockbot tools/matrix
docker run --rm hallow-bedrockbot host.docker.internal 19132
```

These are bots, not game clients: they don't render, fight or build, and a bot library's own bugs can look like server bugs. Confirm failures with a real client before reporting them upstream.
