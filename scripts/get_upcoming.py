import os
import requests
import socket
from datetime import datetime, timedelta

def load_env(path):
    """Load .env file into a dict."""
    env = {}
    if os.path.exists(path):
        with open(path, 'r') as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith('#') and '=' in line:
                    k, v = line.split('=', 1)
                    env[k.strip()] = v.strip().strip('"').strip("'")
    return env

def resolve_url(url):
    """Try to resolve the URL's hostname; fall back to localhost if unreachable."""
    try:
        hostname = url.split("//")[-1].split(":")[0]
        socket.gethostbyname(hostname)
        return url
    except Exception:
        return "http://localhost:8989" if "sonarr" in url else "http://localhost:7878"

def get_upcoming():
    """Fetch upcoming releases from Sonarr and Radarr for the next 7 days."""
    
    # Load .env from multiple locations
    env_paths = [
        r"C:\Users\achraf youssef\Documents\my-projects\homelab\.env",
        os.path.join(os.getcwd(), ".env"),
        os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".env"),
    ]
    
    env = {}
    for path in env_paths:
        if os.path.exists(path):
            env.update(load_env(path))
            break
    
    SONARR_KEY = env.get("SONARR_API_KEY") or os.getenv("SONARR_API_KEY")
    RADARR_KEY = env.get("RADARR_API_KEY") or os.getenv("RADARR_API_KEY")
    
    # Use URLs from .env, fall back to localhost
    sonarr_base = resolve_url(env.get("SONARR_INTERNAL_URL", "http://localhost:8989"))
    radarr_base = resolve_url(env.get("RADARR_INTERNAL_URL", "http://localhost:7878"))

    def normalize_date(d):
        if isinstance(d, str) and 'T' in d:
            return d.split('T')[0]
        return str(d)

    def get_calendar(base, key, start_date, end_date, include_series=False):
        """Fetch calendar endpoint with optional includeSeries param."""
        try:
            params = {"apiKey": key, "startDate": start_date, "endDate": end_date}
            if include_series:
                params["includeSeries"] = "true"
            r = requests.get(
                f"{base}/api/v3/calendar",
                params=params,
                timeout=10
            )
            if r.status_code == 200:
                data = r.json()
                if isinstance(data, list):
                    return data
                elif isinstance(data, dict):
                    return data.get('items', [])
        except Exception as e:
            print(f"Error fetching calendar from {base}: {e}")
        return []

    # Date range: Monday to Sunday of the current week
    # Gateway pads by 1 day on each side for timezone conversion
    # We extend target range by 1 day to catch timezone edge cases
    today = datetime.now()
    days_since_monday = today.weekday()  # 0=Monday, 6=Sunday
    monday = today - timedelta(days=days_since_monday)
    sunday = monday + timedelta(days=6)
    start_date = (monday - timedelta(days=2)).strftime('%Y-%m-%d')
    end_date = (sunday + timedelta(days=2)).strftime('%Y-%m-%d')
    target_start = (monday - timedelta(days=1)).strftime('%Y-%m-%d')
    target_end = (sunday + timedelta(days=1)).strftime('%Y-%m-%d')
    
    # Fetch releases (Sonarr with includeSeries=true to get series title directly)
    sonarr_releases = get_calendar(sonarr_base, SONARR_KEY, start_date, end_date, include_series=True)
    radarr_releases = get_calendar(radarr_base, RADARR_KEY, start_date, end_date)
    
    all_releases = []
    
    # Process Sonarr releases (TV series)
    for item in sonarr_releases:
        air_date_raw = item.get('airDate', '')
        air_date = normalize_date(air_date_raw)
        
        # Filter to target date range
        if air_date < target_start or air_date > target_end:
            continue
        
        # Get series title (available when includeSeries=true)
        series = item.get('series', {})
        title = series.get('title', '') if series else item.get('title', 'Unknown')
        episode_title = item.get('title', '')
        
        season = str(item.get('seasonNumber', '') or '').zfill(2) if item.get('seasonNumber') else ''
        episode = str(item.get('episodeNumber', '') or '').zfill(2) if item.get('episodeNumber') else ''
        
        all_releases.append({
            'date': air_date,
            'title': title,
            'episode_title': episode_title,
            'season': season,
            'episode': episode,
            'kind': 'episode'
        })
    
    # Process Radarr releases (movies)
    for item in radarr_releases:
        # Check date fields in order (matching gateway logic)
        release_date = None
        for field in ['releaseDate', 'digitalRelease', 'physicalRelease', 'inCinemas']:
            val = item.get(field)
            if val:
                release_date = normalize_date(val)
                break
        
        if not release_date:
            continue
        
        # Filter to target date range
        if release_date < target_start or release_date > target_end:
            continue
        
        all_releases.append({
            'date': release_date,
            'title': item.get('title', 'Unknown'),
            'episode_title': '',
            'season': '',
            'episode': '',
            'kind': 'movie'
        })

    # Sort by date, then title, then season, then episode (matching gateway)
    all_releases.sort(key=lambda r: (r['date'], r['title'], r['season'], r['episode']))
    
    # Group releases by date
    releases_by_date = {}
    for release in all_releases:
        date = release['date']
        if date not in releases_by_date:
            releases_by_date[date] = []
        releases_by_date[date].append(release)
    
    sorted_dates = sorted(releases_by_date.keys())
    
    # Format output matching dashboard style
    lines = []
    
    # Week header: "Week 46 | Nov 9–15, 2026"
    week_num = monday.isocalendar()[1]
    lines.append(f"Week {week_num} | {monday.strftime('%b')} {monday.day}\u2013{sunday.day}, {sunday.strftime('%Y')}")
    
    # Stats line: "5 Episodes | 1 Movie"
    episode_count = sum(1 for r in all_releases if r['kind'] == 'episode')
    movie_count = sum(1 for r in all_releases if r['kind'] == 'movie')
    lines.append(f"{episode_count} Episodes | {movie_count} Movie{'s' if movie_count != 1 else ''}")
    
    lines.append("")  # blank line after stats
    
    for date in sorted_dates:
        items = releases_by_date[date]
        
        # Date format: "Wed, Nov 11" (no year)
        try:
            dt = datetime.strptime(date, '%Y-%m-%d')
            formatted_date = dt.strftime('%a, %b %d')
        except:
            formatted_date = date
        
        lines.append(formatted_date)
        
        for item in items:
            title = item['title']
            if item['kind'] == 'movie':
                lines.append(f"MOVIE {title}")
            else:
                # Format: TV {title} S06E09 (with S prefix for season)
                lines.append(f"TV {title} S{item['season']}E{item['episode']}")
        
        lines.append("")  # blank line after each date group
    
    if not sorted_dates:
        return "No upcoming releases found in the next 7 days."
    
    return "\n".join(lines)

if __name__ == "__main__":
    print(get_upcoming())
