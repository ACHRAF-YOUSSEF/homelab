FROM tecnativa/docker-socket-proxy:v0.5.0

# Retain the upstream API allowlist while accommodating long AIO shutdowns.
RUN apk add --no-cache curl \
    && addgroup -S -g 33 aio \
    && mkdir -p /proxy \
    && chown haproxy:aio /proxy \
    && chmod 2770 /proxy \
    && sed -i \
        -e 's/timeout client 10m/timeout client 1h/' \
        -e 's/timeout server 10m/timeout server 1h/' \
        -e 's|pidfile /run/haproxy.pid|pidfile /tmp/haproxy.pid|' \
        /usr/local/etc/haproxy/haproxy.cfg.template \
    && grep -q 'timeout client 1h' /usr/local/etc/haproxy/haproxy.cfg.template \
    && grep -q 'timeout server 1h' /usr/local/etc/haproxy/haproxy.cfg.template \
    && grep -q 'pidfile /tmp/haproxy.pid' /usr/local/etc/haproxy/haproxy.cfg.template

# UID 99 is the upstream HAProxy user; GID 33 shares the frontend with AIO.
USER haproxy:aio
