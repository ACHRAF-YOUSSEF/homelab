FROM tecnativa/docker-socket-proxy:v0.5.0

# Retain the upstream API allowlist while accommodating long AIO shutdowns.
RUN apk add --no-cache curl \
    && sed -i \
        -e 's/timeout client 10m/timeout client 1h/' \
        -e 's/timeout server 10m/timeout server 1h/' \
        /usr/local/etc/haproxy/haproxy.cfg.template \
    && grep -q 'timeout client 1h' /usr/local/etc/haproxy/haproxy.cfg.template \
    && grep -q 'timeout server 1h' /usr/local/etc/haproxy/haproxy.cfg.template
