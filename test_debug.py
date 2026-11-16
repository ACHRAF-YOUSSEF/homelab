import requests, os

env_paths = [
    os.path.join(os.getcwd(), ".env"),
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".env")
]
keys = {}
for path in env_paths:
    if os.path.exists(path):
        with open(path, 'r') as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith('#') and '=' in line:
                    k, v = line.split('=', 1)
                    keys[k.strip()] = v.strip().strip('"').strip("'")
        break

SONARR_KEY = keys.get("SONARR_API_KEY") or os.getenv("SONARR_API_KEY")

r = requests.get("http://localhost:8989/api/v3/calendar", params={"apiKey": SONARR_KEY}, timeout=10)
if r.status_code != 200:
    print(f"API Error: {r.status_code}")
else:
    data = r.json() if isinstance(r.json(), list) else r.json().get('items', [])
    print(f"Total items: {len(data)}")
    for i, item in enumerate(data[:3]):
        print(f"\n=== Item {i} ===")
        # Show all keys with non-empty values
        for k, v in item.items():
            if v is not None and str(v) != '' and not k.startswith('_'):
                print(f"  {k}: {v}")
