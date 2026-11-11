#!/bin/bash
# Homelab Daily Update Script
# Runs docker compose pull, updates containers, cleans up old images,
# and generates a system overview report.

set -u

WORKDIR="/c/Users/achraf youssef/Documents/my-projects/homelab"
LOG_FILE="$WORKDIR/scripts/homelab-daily.log"
TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

# Clear previous log
> "$LOG_FILE"

# Initialize log
echo "========================================" >> "$LOG_FILE"
echo "[$TIMESTAMP] HOMELAB DAILY UPDATE" >> "$LOG_FILE"
echo "========================================" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 1: Save old image state
echo "--- STEP 1: Saving old image state ---" >> "$LOG_FILE"
docker images --format "{{.Repository}}:{{.Tag}} {{.ID}} {{.Size}} {{.CreatedSince}}" > "$WORKDIR/scripts/old-images.tmp" 2>/dev/null
echo "" >> "$LOG_FILE"

# Step 2: Pull latest images
echo "--- STEP 2: Pulling latest images ---" >> "$LOG_FILE"
cd "$WORKDIR"
PULL_OUTPUT=$(docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml -f compose/subforge.cuda.yml pull 2>&1)
PULL_EXIT=$?
echo "$PULL_OUTPUT" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 3: Rebuild and start containers
echo "--- STEP 3: Rebuilding and starting containers ---" >> "$LOG_FILE"
UP_OUTPUT=$(docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml -f compose/subforge.cuda.yml up -d --build 2>&1)
UP_EXIT=$?
echo "$UP_OUTPUT" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 4: Save new image state and compare
echo "--- STEP 4: Comparing image changes ---" >> "$LOG_FILE"
docker images --format "{{.Repository}}:{{.Tag}} {{.ID}} {{.Size}} {{.CreatedSince}}" > "$WORKDIR/scripts/new-images.tmp" 2>/dev/null

# Find changed images (different image ID)
echo "" >> "$LOG_FILE"
echo "=== CHANGED IMAGES ===" >> "$LOG_FILE"
CHANGED_IMAGES=""
while IFS= read -r line; do
    repo_tag=$(echo "$line" | awk '{print $1}')
    new_id=$(echo "$line" | awk '{print $2}')
    old_line=$(grep "^$repo_tag " "$WORKDIR/scripts/old-images.tmp" 2>/dev/null || true)
    if [ -n "$old_line" ]; then
        old_id=$(echo "$old_line" | awk '{print $2}')
        if [ "$old_id" != "$new_id" ]; then
            old_size=$(echo "$old_line" | awk '{print $3}')
            old_created=$(echo "$old_line" | awk '{print $4}')
            new_size=$(echo "$line" | awk '{print $3}')
            new_created=$(echo "$line" | awk '{print $4}')
            echo "  $repo_tag" >> "$LOG_FILE"
            echo "    Old: $old_id ($old_size, $old_created)" >> "$LOG_FILE"
            echo "    New: $new_id ($new_size, $new_created)" >> "$LOG_FILE"
            CHANGED_IMAGES="${CHANGED_IMAGES}  ✏️ $repo_tag: $old_id → $new_id\n"
        else
            echo "  $repo_tag: unchanged ($new_id)" >> "$LOG_FILE"
        fi
    else
        echo "  $repo_tag: NEW IMAGE ($new_id, $new_size)" >> "$LOG_FILE"
        CHANGED_IMAGES="${CHANGED_IMAGES}  🆕 $repo_tag: $new_id ($new_size)\n"
    fi
done < "$WORKDIR/scripts/new-images.tmp"
echo "" >> "$LOG_FILE"

# Step 5: Remove old/dangling images
echo "--- STEP 5: Cleaning up old images ---" >> "$LOG_FILE"
# Find images that were replaced
OLD_IDS=$(comm -23 <(awk '{print $2}' "$WORKDIR/scripts/old-images.tmp" | sort) <(awk '{print $2}' "$WORKDIR/scripts/new-images.tmp" | sort))
if [ -n "$OLD_IDS" ]; then
    for old_id in $OLD_IDS; do
        old_repo=$(grep " $old_id " "$WORKDIR/scripts/old-images.tmp" | awk '{print $1}')
        if [ -n "$old_repo" ]; then
            echo "  Removing old image: $old_repo ($old_id)" >> "$LOG_FILE"
            docker rmi "$old_repo" 2>>"$LOG_FILE" || echo "    (could not remove - may be in use)" >> "$LOG_FILE"
        fi
    done
else
    echo "  No old images to remove." >> "$LOG_FILE"
fi

# Remove dangling images
echo "" >> "$LOG_FILE"
echo "  Removing dangling images..." >> "$LOG_FILE"
docker image prune -f >>"$LOG_FILE" 2>&1 || echo "  (no dangling images to remove)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 6: System overview
echo "--- STEP 6: SYSTEM OVERVIEW ---" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### 📊 Disk Usage (C: Drive)" >> "$LOG_FILE"
df -h /c 2>/dev/null | grep -v tmpfs >> "$LOG_FILE" || echo "  (Windows path)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### 💾 Docker Disk Usage" >> "$LOG_FILE"
docker system df 2>/dev/null | tee -a "$LOG_FILE" >> "$LOG_FILE" || echo "  (unavailable)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### 🧠 Memory Usage" >> "$LOG_FILE"
if command -v free &>/dev/null; then
    free -h 2>/dev/null | head -2 >> "$LOG_FILE"
else
    echo "  (unavailable on Windows)" >> "$LOG_FILE"
fi
echo "" >> "$LOG_FILE"

echo "### 🐳 Running Containers" >> "$LOG_FILE"
echo "  Total: $(docker ps -q | wc -l)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"
docker ps --format "table {{.Names}}\t{{.Image}}\t{{.Status}}" 2>/dev/null >> "$LOG_FILE" || echo "  (unavailable)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### 📦 Docker Images Summary" >> "$LOG_FILE"
echo "  Total images: $(docker images -q | wc -l)" >> "$LOG_FILE"
echo "  Total size: $(docker system df --format '{{.Size}}' 2>/dev/null | head -1 || echo 'N/A')" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "========================================" >> "$LOG_FILE"
echo "[$TIMESTAMP] UPDATE COMPLETE" >> "$LOG_FILE"
echo "========================================" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Clean up temp files
rm -f "$WORKDIR/scripts/old-images.tmp" "$WORKDIR/scripts/new-images.tmp"

# Output only a compact summary for the agent
echo "========================================"
echo "[$TIMESTAMP] HOMELAB DAILY UPDATE"
echo "========================================"
echo ""

# Check for errors
if [ "$PULL_EXIT" -ne 0 ] || [ "$UP_EXIT" -ne 0 ]; then
    echo "⚠️ ERRORS DETECTED - see log: $LOG_FILE"
fi

# Summarize changes
echo ""
echo "=== CHANGED IMAGES ==="
grep -A2 "Old:" "$LOG_FILE" 2>/dev/null | grep -B1 "New:" | head -20
echo ""

# System overview (compact)
echo "=== SYSTEM OVERVIEW ==="
echo "Disk: $(df -h /c 2>/dev/null | awk 'NR==2{print $3"/"$2}' || echo 'N/A')"
echo "Docker: $(docker system df --format '{{.Size}}' 2>/dev/null | head -1 || echo 'N/A')"
echo "Containers: $(docker ps -q | wc -l) running"
echo "Images: $(docker images -q | wc -l)"
echo ""
echo "Full log: $LOG_FILE"
echo "========================================"
