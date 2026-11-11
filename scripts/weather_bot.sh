#!/usr/bin/env bash
# weather_bot.sh - Bash version of weather_bot.py
# Fetches current weather from wttr.in for a given location

# Default location (can be overridden via command line argument)
LOCATION="${1:-Hammam Sousse, Tunisia}"

# Simplify the location for the URL (replace spaces with +)
CLEAN_LOC=$(echo "$LOCATION" | tr ' ' '+')

# Build URL
URL="wttr.in/${CLEAN_LOC}?format=j1"

# Fetch weather data using curl
WEATHER_DATA=$(curl -s --max-time 10 "${URL}" || true)

# Check if we got a valid response
if [[ $(echo "$WEATHER_DATA" | grep -q '"current_condition"' && echo 1) == 0 ]]; then
    echo "Error fetching weather: Failed to connect or no data received"
    exit 1
fi

# Parse JSON using jq
TEMP=$(echo "$WEATHER_DATA" | jq -r '.current_condition[0].temp_C' 2>/dev/null || echo "N/A")
DESCRIPTION=$(echo "$WEATHER_DATA" | jq -r '.current_condition[0].weatherDesc[0].value' 2>/dev/null || echo "N/A")
HUMIDITY=$(echo "$WEATHER_DATA" | jq -r '.current_condition[0].humidity' 2>/dev/null || echo "N/A")

# Handle N/A cases
if [[ "$TEMP" == "null" || -z "$TEMP" ]]; then
    TEMP="N/A"
fi
if [[ "$HUMIDITY" == "null" || -z "$HUMIDITY" ]]; then
    HUMIDITY="N/A"
fi

# Format and output the weather information
echo "The current weather in ${LOCATION} is ${DESCRIPTION} with a temperature of ${TEMP}°C and ${HUMIDITY}% humidity."
