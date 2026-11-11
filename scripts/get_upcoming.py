import os
import requests
from datetime import datetime, timedelta

def get_upcoming():
    env_paths = [
        r"C:\Users\achraf youssef\Documents\my-projects\homelab\.env",
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
    RADARR_KEY = keys.get("RADARR_API_KEY") or os.getenv("RADARR_API_KEY")

    sonarr_base = "http://localhost:8989"
    radarr_base = "http://localhost:7878"
    results = []

    def normalize_date(d):
        return d.split('T')[0] if isinstance(d, str) and 'T' in d else str(d)

    def fetch_series_name(base, key, series_id):
        try:
            r = requests.get(f"{base}/api/v3/series/{series_id}", params={"apiKey": key}, timeout=5)
            if r.status_code == 200:
                data = r.json()
                return data.get('title', '')
        except:
            pass
        return ''

    # Use the next 7 days
    start_date = datetime.now().strftime('%Y-%m-%d')
    end_date = (datetime.now() + timedelta(days=7)).strftime('%Y-%m-%d')

    for base, key, name in [(sonarr_base, SONARR_KEY, "Sonarr"), (radarr_base, RADARR_KEY, "Radarr")]:
        try:
            r = requests.get(f"{base}/api/v3/calendar", params={"apiKey": key, "startDate": start_date, "endDate": end_date}, timeout=10)
            if r.status_code == 200:
                items = r.json() if isinstance(r.json(), list) else r.json().get('items', [])
                for i in items:
                    date = normalize_date(i.get('airDate', i.get('releaseDate', 'Unknown')))
                    title = i.get('title', 'Unknown')
                    if name == "Sonarr":
                        series = fetch_series_name(sonarr_base, SONARR_KEY, i.get('seriesId')) or ''
                        season = i.get('seasonNumber', '')
                        episode = i.get('episodeNumber', '')
                        results.append(f"[{name}] {series} S{season:02d}E{episode:02d} - {title} ({date})")
                    else:
                        results.append(f"[{name}] {title} ({date})")
        except Exception as e:
            results.append(f"[{name}] Error: {e}")

    if not results:
        return "No upcoming releases found in the next 7 days."
    return "\n".join(results)

if __name__ == "__main__":
    print(get_upcoming())
