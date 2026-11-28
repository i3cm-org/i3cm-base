# Build the catalog from the TypeScript type definitions
FROM node:22-bookworm-slim AS catalog
WORKDIR /src
COPY tools/typecat/package.json tools/typecat/package-lock.json tools/typecat/
RUN cd tools/typecat && npm ci --no-audit --no-fund
COPY tools/typecat tools/typecat
COPY types types
RUN node tools/typecat/typecat.mjs types --out /catalog.json

# Build the server
FROM rust:1-bookworm AS build
# build stamp: docker build --build-arg I3CM_REV=$(git rev-parse --short=12 HEAD)
ARG I3CM_REV=unknown
ENV I3CM_REV=$I3CM_REV
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY wit wit
COPY deploy/keys deploy/keys
RUN cargo build --release -p i3cm-server -p i3cm-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 i3cm && useradd --system --uid 10001 --gid i3cm --home-dir /var/lib/i3cm --shell /usr/sbin/nologin i3cm \
 && mkdir -p /var/lib/i3cm/modules && chown -R i3cm:i3cm /var/lib/i3cm
# node and typecat for source checks in development tenants (POST /v1/catalog/check)
COPY --from=catalog /usr/local/bin/node /usr/local/bin/node
COPY --from=catalog /src/tools/typecat /opt/i3cm/typecat
COPY --from=catalog /src/types /opt/i3cm/types
COPY --from=build /src/target/release/i3cm-server /usr/local/bin/i3cm-server
# the agent program (tools, i3cm-init; FUSE needs /dev/fuse on the host)
COPY --from=build /src/target/release/i3cm /usr/local/bin/i3cm
COPY --from=catalog /catalog.json /etc/i3cm/catalog.json
# mandatory modules (tasks, dms), bundled with tools/bundle-modules.sh; copied at start into the
# modules directory, which is state and belongs on a volume (installed modules go there too)
COPY deploy/modules /opt/i3cm/bundled-modules
ENV I3CM_CATALOG=/etc/i3cm/catalog.json I3CM_MODULES_DIR=/var/lib/i3cm/modules I3CM_BUNDLED_MODULES=/opt/i3cm/bundled-modules \
    I3CM_LISTEN=0.0.0.0:8480 I3CM_TYPECAT=/opt/i3cm/typecat/typecat.mjs I3CM_TYPES_DIR=/opt/i3cm/types
VOLUME /var/lib/i3cm/modules
USER 10001:10001
EXPOSE 8480
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 CMD ["i3cm-server", "health"]
ENTRYPOINT ["i3cm-server"]
CMD ["serve"]
