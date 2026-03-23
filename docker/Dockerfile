# Build stage
FROM rust:1.93-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates

# Build release binary
RUN cargo build --release

# Runtime stage
FROM alpine:3.20

RUN apk add --no-cache ca-certificates tzdata

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/aust_backend /app/aust_backend

# Copy config and migrations
COPY config ./config
COPY migrations ./migrations

# Create non-root user
RUN adduser -D -u 1000 aust
USER aust

EXPOSE 8080

ENV RUN_MODE=production

CMD ["/app/aust_backend"]
