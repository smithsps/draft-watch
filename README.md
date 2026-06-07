# League of Legends Draft Watcher
_Basic system tray app that watches and records champ select via the LCU API_

Records raw LCU API states in NDJSON format in `%APPDATA%\DraftWatch\`
```
{"seq": 0, "ts": "2026-06-06T12:00:00.000000000Z", "event": { ...raw LCU JSON... }}
```

Then supports uploading this to a API for data collection and anaylsis. 
