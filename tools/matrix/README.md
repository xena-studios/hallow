# Version matrix clients

Headless clients for the client table in [Validation](../../docs/VALIDATION.md#client-versions). Start the dev network with the multiversion plugin loaded first:

```sh
HALLOW_MULTIVERSION=1 docker compose -f deploy/compose/compose.yaml up -d --build
```

**Java 1.8.9 to 26.1** (mineflayer): join, chat, tab-complete `/ser`, switch to `game-1` and back, try `/server nowhere`. Prints one JSON line.

```sh
cd tools/matrix && npm install --ignore-scripts
for v in 1.8.9 1.12.2 1.16.5 1.20.1 1.20.4 1.21.11 26.1; do node java.js $v; done
node java-1.7.js        # 1.7.10, through minecraft-protocol
```

26.3 is `netcheck` (`cargo run --release -p netcheck -- --route game-1,lobby`).

**Bedrock** through Geyser: run Geyser Standalone with `java.address` set to the proxy, `auth-type: offline`, `bedrock.transport: both` (the bot speaks RakNet) and `advanced.validate-bedrock-login: false`, then:

```sh
docker build -t hallow-bedrockbot tools/matrix
docker run --rm hallow-bedrockbot host.docker.internal 19132
```

These are bots, not game clients: they don't render, fight or build, and a bot library's own bugs can look like server bugs. Confirm failures with a real client before reporting them upstream.
