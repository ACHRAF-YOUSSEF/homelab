import sys
import subprocess
import json

def get_weather(location):
    try:
        # Simplify the location for the URL
        clean_loc = location.replace(" ", "+")
        # Use curl to get JSON from wttr.in
        # curl -s wttr.in/Hammam+Sousse,Tunisia?format=j1
        url = f"wttr.in/{clean_loc}?format=j1"
        
        result = subprocess.check_output(
            ["curl", "-s", url],
            stderr=subprocess.STDOUT
        ).decode('utf-8')
        
        data = json.loads(result)
        current = data.get('current_condition', {})
        temp = current.get('temp_C', 'N/A')
        desc = current.get('weatherDesc', ['N/A'])[0]
        humidity = current.get('humidity', 'N/A')
        
        return f"The current weather in {location} is {desc} with a temperature of {temp}°C and {humidity}% humidity."
    except Exception as e:
        return f"Error fetching weather: {str(e)}"

if __name__ == "__main__":
    # Default location
    loc = sys.argv[1] if len(sys.argv) > 1 else "Hammam Sousse, Tunisia"
    print(get_weather(loc))
