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
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p base-server -p base-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/base-server /usr/local/bin/base-server
# the agent program (tools, base-init; FUSE needs /dev/fuse on the host)
COPY --from=build /src/target/release/base /usr/local/bin/base
COPY --from=catalog /catalog.json /etc/base-erp/catalog.json
ENV BASE_CATALOG=/etc/base-erp/catalog.json BASE_LISTEN=0.0.0.0:8480
EXPOSE 8480
ENTRYPOINT ["base-server"]
CMD ["serve"]
