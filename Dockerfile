FROM rust:1.85 AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y npm && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cd web && npm install && npm run build
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/reader /usr/local/bin/reader
ENV DATA_DIR=/data PORT=5888
VOLUME /data
EXPOSE 5888
ENTRYPOINT ["reader"]
