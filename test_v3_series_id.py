import requests, os, json

env_paths = [
    r"C:\Users\achraf youssef\Documents\my-projects\homelab\.env",
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
    
    # Test /api/v3/series/{id} endpoint
    series_id = str(data[0]['seriesId'])
    v3r_r = requests.get(f"http://localhost:8989/api/v3/series/{series_id}", params={"apiKey": SONARR_KEY}, timeout=5)
    print(f"v3 /api/v3/series/{{id}} status: {v3r_r.status_code}")
    if v3r_r.status_code == 200:
        print(f"Response: {json.dumps(v3r_r.json(), indent=2)[:500]}")
