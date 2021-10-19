FROM rust:latest as builder

WORKDIR /build
COPY . .

RUN cargo install --path .

FROM debian:buster-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/github-matrix-project /usr/local/bin/github-matrix-project
CMD ["github-matrix-project"]
