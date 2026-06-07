# League of Legends Draft Watcher
_Basic system tray app that watches and records champ select via the LCU API_

Records raw LCU API states in NDJSON format in `%APPDATA%\DraftWatch\`
```
{"seq": 0, "ts": "2026-06-06T12:00:00.000000000Z", "event": { ...raw LCU JSON... }}
```

Then supports uploading this to a API for data collection and anaylsis. 

config.toml:
```
  league_path = "C:\\Riot Games\\League of Legends" # Some auto discovery for this, if missing checks a few standard locations.
  # Optional
  upload_url = "https://your-site.com/api/sessions"
  upload_api_key = "your-key"  
```
