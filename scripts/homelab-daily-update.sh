#!/bin/bash
# Homelab Daily Update Skill
# Usage: Run the daily homelab maintenance routine automatically or manually.
#
# The workflow:
#   1. docker compose pull (pull latest images)
#   2. docker compose up -d --build (rebuild and start containers)
#   3. docker image prune -f (remove dangling images)
#   4. Generate system overview report (disk, memory, running containers)
#   5. Log all changes to homelab-daily.log
#
# Command: bash /c/Users/achraf youssef/Documents/my-projects/homelab/scripts/homelab-daily-update.sh
#
# Note: The script is designed for Windows with WSL2/Docker Desktop. It logs
# all operations and reports disk/memory usage, changed images, and running containers.

# Pitfalls:
#   - Paths with spaces (e.g., "C:\Users\achraf youssef") require proper quoting in bash.
#   - Use absolute paths for reliability across shell environments.
#   - The script writes to scripts/homelab-daily.log and compares old/new image states.

set -u

WORKDIR="/c/Users/achraf youssef/Documents/my-projects/homelab"
LOG_FILE="$WORKDIR/scripts/homelab-daily.log"
STATE_FILE="$WORKDIR/scripts/homelab-daily-state.json"
TIMESTAMP=$(date '+%Y-%m-%d %H:%M:%S')

# Retry configuration
MAX_RETRIES=3
RETRY_DELAY=10

# Retry function: retries a command on failure with exponential backoff
retry_command() {
    local cmd="$1"
    local label="$2"
    local attempt=1
    local exit_code=1
    
    while [ $attempt -le $MAX_RETRIES ]; do
        echo "  [$label] Attempt $attempt/$MAX_RETRIES..." >> "$LOG_FILE"
        eval "$cmd"
        exit_code=$?
        
        if [ $exit_code -eq 0 ]; then
            echo "  [$label] Success on attempt $attempt" >> "$LOG_FILE"
            return 0
        fi
        
        # Check if error is a timeout/network issue (retryable)
        if echo "$cmd" | grep -q "timeout\|TLS\|connection\|deadline\|deadline"; then
            if [ $attempt -lt $MAX_RETRIES ]; then
                echo "  [$label] Failed (attempt $attempt), retrying in ${RETRY_DELAY}s..." >> "$LOG_FILE"
                sleep "$RETRY_DELAY"
            fi
        fi
        
        attempt=$((attempt + 1))
    done
    
    echo "  [$label] Failed after $MAX_RETRIES attempts (exit code: $exit_code)" >> "$LOG_FILE"
    return $exit_code
}

# Clear previous log
> "$LOG_FILE"

# Initialize log
echo "========================================" >> "$LOG_FILE"
echo "[$TIMESTAMP] HOMELAB DAILY UPDATE" >> "$LOG_FILE"
echo "========================================" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 1: Save old image state (image:tag format)
echo "--- STEP 1: Saving old image state ---" >> "$LOG_FILE"
docker images --format "{{.Repository}}:{{.Tag}}" > "$WORKDIR/scripts/old-images.tmp" 2>/dev/null
echo "" >> "$LOG_FILE"

# Step 2: Pull latest images (with retry on timeout)
echo "--- STEP 2: Pulling latest images ---" >> "$LOG_FILE"
cd "$WORKDIR"
PULL_CMD="docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml -f compose/subforge.cuda.yml pull 2>&1"
PULL_OUTPUT=$(retry_command "$PULL_CMD" "PULL")
PULL_EXIT=$?
echo "$PULL_OUTPUT" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 3: Rebuild and start containers (with retry on timeout)
echo "--- STEP 3: Rebuilding and starting containers ---" >> "$LOG_FILE"
UP_CMD="docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml -f compose/subforge.cuda.yml up -d --build 2>&1"
UP_OUTPUT=$(retry_command "$UP_CMD" "UP")
UP_EXIT=$?
echo "$UP_OUTPUT" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 4: Save new image state (image:tag format)
echo "--- STEP 4: Comparing image changes ---" >> "$LOG_FILE"
docker images --format "{{.Repository}}:{{.Tag}}" > "$WORKDIR/scripts/new-images.tmp" 2>/dev/null

# Find changed images (compare by repo:tag, showing old/new IDs)
echo "" >> "$LOG_FILE"
echo "=== CHANGED IMAGES ===" >> "$LOG_FILE"

# Read new images into associative format: tag -> id
declare -A new_ids
while IFS= read -r img_tag; do
    inspect=$(docker inspect --format='{{.ID}}' "${img_tag}" 2>/dev/null)
    if [ -n "$inspect" ]; then
        new_ids["$img_tag"]="$inspect"
    fi
done < "$WORKDIR/scripts/new-images.tmp"

# Compare old vs new by repo:tag
while IFS= read -r img_tag; do
    # Extract image name without tag for matching (e.g., docker.io/library/nginx -> nginx)
    base_name=$(echo "$img_tag" | sed 's/:.*$//')
    
    # Find matching old entry (same base name, ignore tag differences)
    old_line=$(grep "^${base_name} " "$WORKDIR/scripts/old-images.tmp" 2>/dev/null | head -1 || true)
    
    new_id="${new_ids[$img_tag]:-}"
    if [ -n "$old_line" ]; then
        old_id=$(echo "$old_line" | awk '{print $2}')
        old_size=$(echo "$old_line" | sed 's/.*[[:space:]]\([0-9]*\)$/'\1/')
        old_created=$(echo "$old_line" | sed 's/.*[[:space:]]\([0-9:-]*\)$/'\1/')
        
        if [ -n "$new_id" ] && [ "${new_id:0:1}" = "sha256" ]; then
            new_size=$(echo "$img_tag" | grep -oP "(?<=[[:space:]])([0-9]+\.?[0-9]*)?$" || echo "unknown")
            
            if [ "$old_id" != "$new_id" ]; then
                echo "  CHANGED $img_tag:" >> "$LOG_FILE"
                echo "    Old: $old_id (changed)" >> "$LOG_FILE"
                echo "    New: $new_id" >> "$LOG_FILE"
            else
                echo "  $img_tag: unchanged ($new_id)" >> "$LOG_FILE"
            fi
        else
            # No new ID found or not a real image - treat as new
            echo "  NEW $img_tag: NEW" >> "$LOG_FILE"
        fi
    else
        # New image (not in old list)
        if [ -n "$new_id" ] && [ "${new_id:0:1}" = "sha256" ]; then
            echo "  NEW $img_tag: NEW ($new_id)" >> "$LOG_FILE"
        else
            echo "  $img_tag: present" >> "$LOG_FILE"
        fi
    fi
done < "$WORKDIR/scripts/new-images.tmp"
echo "" >> "$LOG_FILE"

# Step 4b: Compare with previous run state
echo "--- STEP 4b: Comparing with previous run ---" >> "$LOG_FILE"
PREV_STATE_FILE="$STATE_FILE"
# Convert MSYS path to Windows path for Python
WIN_STATE_FILE=$(cygpath -w "$STATE_FILE" 2>/dev/null || echo "$STATE_FILE")
WIN_PREV_STATE_FILE=$(cygpath -w "$PREV_STATE_FILE" 2>/dev/null || echo "$PREV_STATE_FILE")
if [ -f "$PREV_STATE_FILE" ]; then
    echo "" >> "$LOG_FILE"
    echo "=== CHANGES FROM LAST RUN ===" >> "$LOG_FILE"
    
    # Build current state (image:tag -> sha256)
    declare -A current_state
    while IFS= read -r img_tag; do
        inspect=$(docker inspect --format='{{.Id}}' "${img_tag}" 2>/dev/null)
        if [ -n "$inspect" ]; then
            current_state["$img_tag"]="$inspect"
        fi
    done < "$WORKDIR/scripts/new-images.tmp"
    
    # Load previous state from JSON
    declare -A prev_state
    if command -v python3 &>/dev/null; then
        while IFS='=' read -r tag hash; do
            # Strip trailing CR from Windows line endings
            hash="${hash%$'\r'}"
            tag="${tag%$'\r'}"
            if [ -n "$tag" ] && [ -n "$hash" ]; then
                prev_state["$tag"]="$hash"
            fi
        done < <(python3 -c "
import json, sys
try:
    with open(sys.argv[1]) as f:
        data = json.load(f)
    for tag, hash in data.items():
        print(f'{tag}={hash}')
except:
    pass
" "$WIN_PREV_STATE_FILE" 2>/dev/null)
    fi
    
    # Find updated images (same tag, different hash)
    UPDATED=()
    for tag in "${!current_state[@]}"; do
        if [ -n "${prev_state[$tag]:-}" ] && [ "${prev_state[$tag]}" != "${current_state[$tag]}" ]; then
            UPDATED+=("$tag")
        fi
    done
    
    # Find new images (new tag not in previous state)
    NEW=()
    for tag in "${!current_state[@]}"; do
        if [ -z "${prev_state[$tag]:-}" ]; then
            NEW+=("$tag")
        fi
    done
    
    # Find removed images (in previous state but not current)
    REMOVED=()
    for tag in "${!prev_state[@]}"; do
        if [ -z "${current_state[$tag]:-}" ]; then
            REMOVED+=("$tag")
        fi
    done
    
    if [ ${#UPDATED[@]} -gt 0 ]; then
        echo "" >> "$LOG_FILE"
        echo "  Updated (${#UPDATED[@]}):" >> "$LOG_FILE"
        for tag in "${UPDATED[@]}"; do
            echo "    $tag" >> "$LOG_FILE"
        done
    fi
    
    if [ ${#NEW[@]} -gt 0 ]; then
        echo "" >> "$LOG_FILE"
        echo "  New (${#NEW[@]}):" >> "$LOG_FILE"
        for tag in "${NEW[@]}"; do
            echo "    $tag" >> "$LOG_FILE"
        done
    fi
    
    if [ ${#REMOVED[@]} -gt 0 ]; then
        echo "" >> "$LOG_FILE"
        echo "  Removed (${#REMOVED[@]}):" >> "$LOG_FILE"
        for tag in "${REMOVED[@]}"; do
            echo "    $tag" >> "$LOG_FILE"
        done
    fi
    
    if [ ${#UPDATED[@]} -eq 0 ] && [ ${#NEW[@]} -eq 0 ] && [ ${#REMOVED[@]} -eq 0 ]; then
        echo "  No changes from previous run." >> "$LOG_FILE"
    fi
else
    echo "  (No previous state file found - first run)" >> "$LOG_FILE"
fi
echo "" >> "$LOG_FILE"

# Save current state for next run
echo "--- STEP 4c: Saving current state ---" >> "$LOG_FILE"
python3 -c "
import json, subprocess, sys
result = subprocess.run(['docker', 'images', '--format', '{{.Repository}}:{{.Tag}}'], 
                       capture_output=True, text=True)
images = {}
for line in result.stdout.strip().split('\n'):
    if line.strip():
        tag = line.strip()
        inspect = subprocess.run(['docker', 'inspect', '--format', '{{.Id}}', tag], 
                                capture_output=True, text=True)
        if inspect.returncode == 0 and inspect.stdout.strip():
            images[tag] = inspect.stdout.strip()
with open(sys.argv[1], 'w') as f:
    json.dump(images, f, indent=2)
print(f'  Saved state for {len(images)} images to {sys.argv[2]}')
" "$WIN_STATE_FILE" "$WIN_STATE_FILE" >> "$LOG_FILE" 2>&1
echo "" >> "$LOG_FILE"

# Step 5: Remove old/dangling images
echo "--- STEP 5: Cleaning up old images ---" >> "$LOG_FILE"

# Create a set of current image tags
declare -A current_tags
while IFS= read -r tag; do
    if [ -n "$tag" ]; then
        current_tags["$tag"]=1
    fi
done < "$WORKDIR/scripts/new-images.tmp"

# Find tags that existed before but don't exist now (replaced images)
OLD_TAGS=""
while IFS= read -r old_tag; do
    if [ -n "$old_tag" ] && [ -z "${current_tags[$old_tag]:-}" ]; then
        OLD_TAGS="${OLD_TAGS} ${old_tag}"
    fi
done < "$WORKDIR/scripts/old-images.tmp"

if [ -n "$OLD_TAGS" ]; then
    # Remove each old/replaced image by its tag (which now points to the new ID)
    for old_tag in $OLD_TAGS; do
        echo "  Removing replaced image: $old_tag" >> "$LOG_FILE"
        docker rmi "$old_tag" -f 2>>"$LOG_FILE" || echo "    (could not remove - may be in use)" >> "$LOG_FILE"
    done
else
    echo "  No replaced images to remove." >> "$LOG_FILE"
fi

# Remove dangling images
echo "" >> "$LOG_FILE"
echo "  Removing dangling images..." >> "$LOG_FILE"
docker image prune -f >>"$LOG_FILE" 2>&1 || echo "  (no dangling images to remove)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

# Step 6: System overview
echo "--- STEP 6: SYSTEM OVERVIEW ---" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### [DISK] All Disks" >> "$LOG_FILE"
df -h --output=target,size,used,avail,pcent 2>/dev/null | grep -v "Mounted" | grep -v "^$" >> "$LOG_FILE" || echo "  (unavailable)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### [DOCKER DISK] Docker Disk Usage" >> "$LOG_FILE"
docker system df 2>/dev/null | tee -a "$LOG_FILE" >> "$LOG_FILE" || echo "  (unavailable)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### [MEMORY] Memory Usage" >> "$LOG_FILE"
if command -v free &>/dev/null; then
    free -h 2>/dev/null | head -2 >> "$LOG_FILE"
else
    echo "  (unavailable on Windows)" >> "$LOG_FILE"
fi
echo "" >> "$LOG_FILE"

echo "### [CONTAINERS] Running Containers" >> "$LOG_FILE"
echo "  Total: $(docker ps -q | wc -l)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"
docker ps --format "table {{.Names}}\t{{.Image}}\t{{.Status}}" 2>/dev/null >> "$LOG_FILE" || echo "  (unavailable)" >> "$LOG_FILE"
echo "" >> "$LOG_FILE"

echo "### [IMAGES] Docker Images Summary" >> "$LOG_FILE"
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
    echo "WARNING ERRORS DETECTED - see log: $LOG_FILE"
fi

echo ""
echo "=== IMAGE CHANGES ==="
# Extract changed/new images from log (first column = image names)
grep -E "^(CHANGED|NEW)" "$LOG_FILE" 2>/dev/null | sed 's/^[[:space:]]*//' | head -30
echo ""

# Show changes from last run
echo "=== CHANGES FROM LAST RUN ==="
if [ -f "$STATE_FILE" ]; then
    UPDATED=$(awk '/Updated \(/,/^[[:space:]]*$/{if(/^    / && !/Updated/) print}' "$LOG_FILE" 2>/dev/null | sed 's/^[[:space:]]*//' || true)
    NEW=$(awk '/New \(/,/^[[:space:]]*$/{if(/^    / && !/New/) print}' "$LOG_FILE" 2>/dev/null | sed 's/^[[:space:]]*//' || true)
    REMOVED=$(awk '/Removed \(/,/^[[:space:]]*$/{if(/^    / && !/Removed/) print}' "$LOG_FILE" 2>/dev/null | sed 's/^[[:space:]]*//' || true)
    
    if [ -n "$UPDATED" ]; then
        echo "Updated:"
        echo "$UPDATED"
    fi
    if [ -n "$NEW" ]; then
        echo "New:"
        echo "$NEW"
    fi
    if [ -n "$REMOVED" ]; then
        echo "Removed:"
        echo "$REMOVED"
    fi
    if [ -z "$UPDATED" ] && [ -z "$NEW" ] && [ -z "$REMOVED" ]; then
        echo "  No changes from previous run."
    fi
else
    echo "  (No previous state file found - first run)"
fi
echo ""

echo "=== SYSTEM OVERVIEW ==="
echo "Disk:" >> "$WORKDIR/scripts/summary.tmp"
df -h --output=target,size,used,avail,pcent 2>/dev/null | grep -v "Mounted" | grep -v "^$" | while IFS= read -r line; do
    mount=$(echo "$line" | awk '{print $1}')
    size=$(echo "$line" | awk '{print $2}')
    used=$(echo "$line" | awk '{print $3}')
    avail=$(echo "$line" | awk '{print $4}')
    usepct=$(echo "$line" | awk '{print $5}')
    echo "  $mount: $used used / $size ($usepct), $avail avail" >> "$WORKDIR/scripts/summary.tmp"
done
echo "" >> "$WORKDIR/scripts/summary.tmp"
echo "Docker: $(docker system df --format '{{.Size}}' 2>/dev/null | head -1 || echo 'N/A')" >> "$WORKDIR/scripts/summary.tmp"
echo "Containers: $(docker ps -q | wc -l) running" >> "$WORKDIR/scripts/summary.tmp"
echo "Images: $(docker images -q | wc -l)" >> "$WORKDIR/scripts/summary.tmp"
cat "$WORKDIR/scripts/summary.tmp"
rm -f "$WORKDIR/scripts/summary.tmp"
echo ""
echo "Full log: $LOG_FILE"
echo "========================================"
