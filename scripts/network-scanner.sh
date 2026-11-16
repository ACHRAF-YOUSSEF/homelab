#!/bin/bash
# Network Device Discovery Script for Homelab

LOG_FILE="/c/Users/achraf youssef/Documents/my-projects/homelab/scripts/network-discovery.log"

echo "=== NETWORK DEVICE DISCOVERY ===" > "$LOG_FILE"
echo "[$(date '+%Y-%m-%d %H:%M:%S')] Scan started" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# 1. Discover devices from Docker port mappings
echo "--- Running Services ---" >> "$LOG_FILE"
docker ps -a --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" 2>/dev/null | while read line; do
    name=$(echo "$line" | cut -f1)
    status=$(echo "$line" | cut -f2)
    ports=$(echo "$line" | cut -f3)
    
    if [ -n "$name" ] && [ -n "$ports" ]; then
        echo "  [$status] $name: $ports" >> "$LOG_FILE"
    fi
done
echo "" >> "$LOG_FILE"

# 2. Check for AdGuard Home DNS records (connected devices)
if docker exec adguardhome cat /etc/dnsmasq.conf 2>/dev/null | grep -i "server"; then
    echo "--- DNS Resolvers ---" >> "$LOG_FILE"
    docker exec adguardhome cat /etc/dnsmasq.conf 2>/dev/null | grep -i "^server\|^domain\|^dhcp-ignore" >> "$LOG_FILE" || true
fi
echo "" >> "$LOG_FILE"

# 3. Check Nextcloud AIO for device discovery status
if docker exec nextcloud-aio-mastercontainer cat /app/nextcloud-aio/core-configuration.yml 2>/dev/null | grep -i "device-discovery\|network"; then
    echo "--- Device Discovery Status ---" >> "$LOG_FILE"
    docker exec nextcloud-aio-mastercontainer cat /app/nextcloud-aio/core-configuration.yml 2>/dev/null | grep -A5 -B5 "network" >> "$LOG_FILE" || true
fi
echo "" >> "$LOG_FILE"

# 4. Uptime Kuma monitors
if docker exec uptime-kuma curl -s http://localhost:3001/api/status 2>/dev/null > /tmp/kuma-status.json; then
    echo "--- Monitored Devices (Uptime Kuma) ---" >> "$LOG_FILE"
    docker exec uptime-kuma cat /tmp/kuma-status.json 2>/dev/null | python3 -m json.tool 2>/dev/null | grep -E '"hostname":|"ip_address"' >> "$LOG_FILE" || true
fi
echo "" >> "$LOG_FILE"

# 5. Local network info from container
if docker exec subtitle-studio cat /etc/resolv.conf 2>/dev/null; then
    echo "--- Current Network Interface ---" >> "$LOG_FILE"
    docker exec subtitle-studio cat /etc/resolv.conf >> "$LOG_FILE" || true
fi
echo "" >> "$LOG_FILE"

# 6. Summary
echo "=== DISCOVERY SUMMARY ===" >> "$LOG_FILE"
DOCKER_CONTAINERS=$(docker ps -q --format "{{.Names}}" 2>/dev/null | wc -l)
echo "Active Docker Containers: $DOCKER_CONTAINERS" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "Discovery complete."
cat "$LOG_FILE"