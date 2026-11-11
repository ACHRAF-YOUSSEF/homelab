import os
import requests
from datetime import datetime, timedelta

def get_missing_today():
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
    today = datetime.now().strftime('%Y-%m-%d')

    def normalize_date(d):
        return d.split('T')[0] if isinstance(d, str) and 'T' in d else str(d)

    # Check Sonarr calendar for episodes airing today that are missing
    try:
        r = requests.get(f"{sonarr_base}/api/v3/calendar", params={"apiKey": SONARR_KEY}, timeout=10)
        if r.status_code == 200:
            items = r.json() if isinstance(r.json(), list) else r.json().get('items', [])
            for item in items:
                air_date = normalize_date(item.get('airDate', ''))
                if air_date == today and not item.get('hasFile', False):
                    title = item.get('title', 'Unknown')
                    season = item.get('seasonNumber', '')
                    episode = item.get('episodeNumber', '')
                    
                    # Try to get series name from /api/v3/series/{id} endpoint
                    series_name = ''
                    if item.get('seriesId'):
                        try:
                            series_r = requests.get(
                                f"{sonarr_base}/api/v3/series/{item['seriesId']}",
                                params={"apiKey": SONARR_KEY},
                                timeout=5
                            )
                            if series_r.status_code == 200:
                                series_data = series_r.json()
                                series_name = series_data.get('title', '')
                        except:
                            pass
                    
                    results.append(f"[Sonarr] {series_name} S{season:02d}E{episode:02d} - {title} (MISSING)")
    except Exception as e:
        results.append(f"[Sonarr] Error checking: {e}")

    # Check Radarr calendar for movies releasing today that are missing
    try:
        r = requests.get(f"{radarr_base}/api/v3/calendar", params={"apiKey": RADARR_KEY}, timeout=10)
        if r.status_code == 200:
            items = r.json() if isinstance(r.json(), list) else r.json().get('items', [])
            for item in items:
                release_date = normalize_date(item.get('releaseDate', ''))
                if release_date == today and not item.get('hasFile', False):
                    title = item.get('title', 'Unknown')
                    results.append(f"[Radarr] {title} (MISSING)")
    except Exception as e:
        results.append(f"[Radarr] Error checking: {e}")

    if not results:
        return f"No new releases found today ({today}) that are missing from your disk."
    return "\n".join(results)

if __name__ == "__main__":
    print(get_missing_today())
