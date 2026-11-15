# Geyser Standalone for Hallow: Bedrock players join through it and reach
# Vine as Java players. The version and build are pinned and the jar is
# checked against the checksum GeyserMC publishes for that build.
#   docker build -t hallow-geyser deploy/geyser

FROM eclipse-temurin:21-jre-alpine
ARG GEYSER_VERSION=2.12.1
ARG GEYSER_BUILD=1302
ARG GEYSER_SHA256=ccafefeb8f5447396bfc8e5411e5476c59fb84de3b99196b8d1b7de3306e92c8
RUN wget -q -O /opt/geyser.jar \
      "https://download.geysermc.org/v2/projects/geyser/versions/${GEYSER_VERSION}/builds/${GEYSER_BUILD}/downloads/standalone" \
    && echo "${GEYSER_SHA256}  /opt/geyser.jar" | sha256sum -c - \
    && adduser -D -u 2614 geyser \
    && mkdir /geyser && chown geyser /geyser
WORKDIR /geyser
USER geyser
# RakNet and the built-in NetherNet signaling share 19132 (UDP and TCP);
# NetherNet's WebRTC traffic uses 19133/udp.
EXPOSE 19132/udp 19132/tcp 19133/udp
ENTRYPOINT ["java", "-Xms256M", "-Xmx1G", "-jar", "/opt/geyser.jar", "--nogui"]
