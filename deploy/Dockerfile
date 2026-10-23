FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata curl
WORKDIR /app
COPY nexus-broker /app/nexus-broker
RUN chmod +x /app/nexus-broker

ENV NEXUS_PORT_HTTP=8790 \
    NEXUS_PORT_TUNNEL=8791 \
    NEXUS_BIND_ADDR=0.0.0.0 \
    RUST_LOG=info,nexus_mesh=info

EXPOSE 8790
EXPOSE 8791

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD curl -f http://127.0.0.1:8790/v1/mesh/health || exit 1

ENTRYPOINT ["/app/nexus-broker"]
