FROM node:24-alpine AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.94-bookworm AS rust
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY protocol/ protocol/
COPY signaling/ signaling/
COPY host/ host/
RUN cargo build --locked --release -p remote-signaling

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home remote
WORKDIR /app
COPY --from=rust /src/target/release/remote-signaling /usr/local/bin/remote-signaling
COPY --from=web /src/web/dist /app/web/dist
USER remote
EXPOSE 8080
ENTRYPOINT ["remote-signaling"]
