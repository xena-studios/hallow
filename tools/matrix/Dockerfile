# The Bedrock client needs raknet-native, which builds with cmake. Building it
# here keeps cmake off the host.
FROM node:26-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
WORKDIR /bot
COPY package.json .
RUN npm install
COPY *.js .
ENTRYPOINT ["node", "bedrock.js"]
