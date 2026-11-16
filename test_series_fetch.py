import requests, os

# Load env file
env_paths = [
    os.path.join(os.getcwd(), ".env"),
]
keys = {}
for path in env_paths:
    if os.path.exists(path):
        with open(path, 'r') as f:
            for line in f:
                line = line.strip()
                if line and '=' in line:
                    k, v = line.split('=', 1)
                    keys[k.strip()] = v.strip().strip('"').strip("'")
        break

SONARR_KEY = keys.get("SONARR_API_KEY") or os.getenv("SONARR_API_KEY")

r = requests.get("http://localhost:8989/api/v3/calendar", params={"apiKey": SONARR_KEY}, timeout=10)
if r.status_code == 200:
    data = r.json() if isinstance(r.json(), list) else r.json().get('items', [])
    print(f"Total calendar items: {len(data)}")
    
    # Fetch the first item's series details
    if data and 'seriesId' in data[0]:
        series_id = str(data[0]['seriesId'])
        print(f"Fetching series ID: {series_id}")
        
        series_r = requests.get(
            f"http://localhost:8989/api/v3/series",
            params={"apiKey": SONARR_KEY, "seriesId": series_id},
            timeout=5
        )
        print(f"Series request status: {series_r.status_code}")
        
        if series_r.status_code == 200:
            series_data = series_r.json()
            print(f"Response type: {type(series_data)}")
            
            # Handle both dict and list responses
            if isinstance(series_data, dict):
                print(f"Title from dict: {series_data.get('title', 'EMPTY')}")
            elif isinstance(series_data, list) and len(series_data) > 0:
                print(f"Series data list length: {len(series_data)}")
                for s in series_data[:3]:
                    if isinstance(s, dict):
                        print(f"  Title: {s.get('title', 'EMPTY')}")
            else:
                print("Unexpected response format")
        else:
            print(f"Failed to fetch series: {series_r.text[:200]}")
    else:
        print("No seriesId found in calendar item")
