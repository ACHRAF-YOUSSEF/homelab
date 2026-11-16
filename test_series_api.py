import requests, os, json

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
    
    # Test the old v2 endpoint
    v2_r = requests.get(f"http://localhost:8989/api/v2/series/{data[0]['seriesId']}", params={"apiKey": SONARR_KEY}, timeout=5)
    print(f"v2 /api/v2/series/{{id}} status: {v2_r.status_code}")
    if v2_r.status_code == 200:
        print(f"Response: {json.dumps(v2_r.json(), indent=2)[:500]}")
    
    # Also try with seriesSlug
    slug = data[0].get('seriesSlug', '')
    print(f"\nSeries Slug from calendar: {slug}")
