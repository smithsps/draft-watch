# League of Legends Draft Watcher
_Always forgetting the draft order and need to flame your top laner who traded back and counter picked themselves?_

Then like me, this app might be for you. DraftWatch is a basic system tray app that watches and records champ select via the LCU API.

## Features
- Records Bans/Picks/Trades from the OnJsonApiEvent_lol-champ-select_v1_session LCU (League Client Update) API
- View past match champ selects, check back on exactly what went wrong.
- Optionally uploades these to a configurable endpoint for shared data collection


## Other Details
Records raw LCU API states in JSONL format in `%APPDATA%\DraftWatch\`
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

## 🤖 AI Disclosure & Disclaimer

This project was written by a human developer with heavy assistance from AI tools, specifically with Claude Code.