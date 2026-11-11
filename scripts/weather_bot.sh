#!/bin/bash

# Weather Bot - Shell script version using wttr.in
# Retrieves weather data from wttr.in API and formats output

LOCATION="${1:-Hammam Sousse, Tunisia}"

# Fetch weather data from wttr.in and parse with Python inline
RESULT=$(python3 << 'PYTHON_EOF'
import subprocess
import json
import sys

location = "${1:-Hammam Sousse, Tunisia}".replace(' ', '+') if len(sys.argv) > 1 else "Hammam+Sousse,Tunisia"
url = f"https://wttr.in/{location}?format=j1"

try:
    # Use curl via Python subprocess to fetch data
    result = subprocess.run(
        ["curl", "-sS", "-A", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", url],
        capture_output=True, text=True
    )
    
    response = result.stdout
    
    if not response.strip():
        print("N/A|Unknown|N/A")
        sys.exit(0)
    
    try:
        data = json.loads(response)
        temp = str(data.get('current_condition', [{}])[0].get('temp_C', 'N/A'))
        desc = str(data.get('current_condition', [{}])[0].get('weatherDesc', [{}])[0].get('value', 'Unknown')).strip()
        humidity = str(data.get('current_condition', [{}])[0].get('humidity', 'N/A'))
        print(f"{temp}|{desc}|{humidity}")
    except (json.JSONDecodeError, KeyError, IndexError):
        # Fall back to text parsing if JSON fails
        temp = "N/A"
        desc = "Unknown"
        humidity = "N/A"
except Exception as e:
    print(f"N/A|Error:{str(e)[:20]}|N/A")

PYTHON_EOF
)

# Parse the result from Python
TEMP=$(echo "$RESULT" | cut -d'|' -f1)
WEATHER_DESC=$(echo "$RESULT" | cut -d'|' -f2)
HUMIDITY=$(echo "$RESULT" | cut -d'|' -f3)

# Handle error case
if [ "$TEMP" == "N/A" ] && [ "$WEATHER_DESC" == "Unknown" ]; then
    echo "Error: Could not parse weather data from response."
    exit 1
fi

# Format the output
if [ -n "$TEMP" ] && [ -n "$WEATHER_DESC" ] && [ "$TEMP" != "N/A" ]; then
    echo "The current weather in $LOCATION is ${WEATHER_DESC} with a temperature of ${TEMP}°C and ${HUMIDITY}% humidity."
else
    echo "Error: Could not parse weather data from response. Got: TEMP=$TEMP DESC=$WEATHER_DESC HUMIDITY=$HUMIDITY"
fi
