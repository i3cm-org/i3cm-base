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
RUN cargo build --release -p i3cm-server -p i3cm-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/i3cm-server /usr/local/bin/i3cm-server
# the agent program (tools, i3cm-init; FUSE needs /dev/fuse on the host)
COPY --from=build /src/target/release/i3cm /usr/local/bin/i3cm
COPY --from=catalog /catalog.json /etc/i3cm/catalog.json
# mandatory modules (tasks, dms), bundled with tools/bundle-modules.sh; installed modules go here too
COPY deploy/modules /opt/i3cm/modules
ENV I3CM_CATALOG=/etc/i3cm/catalog.json I3CM_MODULES_DIR=/opt/i3cm/modules I3CM_LISTEN=0.0.0.0:8480
EXPOSE 8480
ENTRYPOINT ["i3cm-server"]
CMD ["serve"]
