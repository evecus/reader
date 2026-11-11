# Build stage: compile the web client (Node) and the server (cargo, musl).
FROM rust:1.83-alpine AS rust-build
RUN apk add --no-cache musl-dev nodejs npm
WORKDIR /build

# Web client: a build-time artifact embedded via rust-embed; the runtime
# image contains the single executable and nothing else.
COPY web/package.json web/package-lock.json ./web/
RUN cd web && npm ci --no-audit --no-fund
COPY web ./web
RUN cd web && npm run build

# Cargo dependency cache layer, then the real build.
COPY Cargo.toml Cargo.lock ./
COPY schema.sql migrations.sql ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && cargo build --release
COPY src ./src
RUN touch src/main.rs schema.sql migrations.sql && cargo build --release

# Runtime: scratch image, one static binary, two volumes.
FROM scratch
COPY --from=rust-build /build/target/release/reader /reader
# The bare binary keeps the Node build's default PORT=5888 for drop-in
# compatibility; the image opts into 8080 so -p 8080:8080 works unchanged.
ENV PORT=8080
EXPOSE 8080
VOLUME ["/books", "/data"]
USER 100:100
ENTRYPOINT ["/reader"]
HEALTHCHECK --interval=30s --timeout=5s CMD ["/reader", "healthcheck"]
