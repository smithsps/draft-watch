use axum::{extract::Path, response::Html, routing::get, Router};
use chrono::DateTime;
use serde_json::Value;
use std::{collections::HashMap, fs, path::PathBuf};
use tokio::net::TcpListener;

// ── Champion data from Data Dragon CDN ───────────────────────────────────────

struct ChampionInfo {
    name: String,
    key: String, // DDragon "id" field used in image URLs (e.g. "MissFortune")
}

struct ChampionData {
    version: String,
    by_id: HashMap<u32, ChampionInfo>,
}

static CHAMP_DATA: std::sync::OnceLock<ChampionData> = std::sync::OnceLock::new();

async fn fetch_champion_data() -> anyhow::Result<ChampionData> {
    let client = reqwest::Client::new();

    let versions: Vec<String> = client
        .get("https://ddragon.leagueoflegends.com/api/versions.json")
        .send()
        .await?
        .json()
        .await?;

    let version = versions
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty versions list"))?;

    let data: Value = client
        .get(format!(
            "https://ddragon.leagueoflegends.com/cdn/{version}/data/en_US/champion.json"
        ))
        .send()
        .await?
        .json()
        .await?;

    let mut by_id = HashMap::new();
    if let Some(champs) = data["data"].as_object() {
        for (_, c) in champs {
            let Some(key_str) = c["key"].as_str() else { continue };
            let Some(name) = c["name"].as_str() else { continue };
            let Some(ddr_id) = c["id"].as_str() else { continue };
            let Ok(numeric_id) = key_str.parse::<u32>() else { continue };
            by_id.insert(
                numeric_id,
                ChampionInfo {
                    name: name.to_string(),
                    key: ddr_id.to_string(),
                },
            );
        }
    }

    Ok(ChampionData { version, by_id })
}

fn champ_name(id: u32) -> &'static str {
    CHAMP_DATA
        .get()
        .and_then(|d| d.by_id.get(&id))
        .map(|c| c.name.as_str())
        .unwrap_or("Unknown")
}

fn champ_img_url(id: u32) -> Option<String> {
    let d = CHAMP_DATA.get()?;
    let info = d.by_id.get(&id)?;
    Some(format!(
        "https://ddragon.leagueoflegends.com/cdn/{}/img/champion/{}.png",
        d.version, info.key
    ))
}

// ─────────────────────────────────────────────────────────────────────────────

pub async fn start() -> u16 {
    match fetch_champion_data().await {
        Ok(data) => {
            CHAMP_DATA.set(data).ok();
        }
        Err(e) => tracing::warn!("Failed to fetch champion data: {e}"),
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("viewer: bind failed");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app()).await.ok();
    });
    port
}

fn app() -> Router {
    Router::new()
        .route("/", get(list_sessions))
        .route("/session/:filename", get(session_detail))
}

async fn list_sessions() -> Html<String> {
    Html(render_list())
}

async fn session_detail(Path(filename): Path<String>) -> Html<String> {
    if !is_valid_filename(&filename) {
        return Html(error_page("Invalid session filename."));
    }
    Html(render_session(&filename))
}

fn is_valid_filename(name: &str) -> bool {
    name.ends_with(".jsonl")
        && name.len() > 6
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn sessions_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("DraftWatch")
        .join("sessions")
}

// Returns (first-line timestamp, last-line event value) without reading the file twice.
fn read_session_meta(path: &PathBuf) -> (Option<DateTime<chrono::Utc>>, Option<Value>) {
    let Ok(content) = fs::read_to_string(path) else {
        return (None, None);
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();

    let dt = lines.first()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["ts"].as_str().map(str::to_string))
        .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
        .map(Into::into);

    let last_event = lines.last()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v.get("event").cloned());

    (dt, last_event)
}

// ── List page ────────────────────────────────────────────────────────────────

fn queue_name(id: u32) -> &'static str {
    match id {
        420 => "Ranked Solo/Duo",
        440 => "Ranked Flex",
        400 => "Normal Draft",
        430 => "Normal Blind",
        450 => "ARAM",
        900 => "ARURF",
        1020 => "One for All",
        1300 => "Nexus Blitz",
        1700 => "Arena",
        3140 => "Practice Tool",
        _ => "Custom",
    }
}

fn render_list() -> String {
    struct Entry {
        dt: DateTime<chrono::Utc>,
        name: String,
        queue: &'static str,
        champ_img: String,
        champ_pos: String,
        player_name: String,
        game_id: Option<i64>,
        is_complete: bool,
    }

    let dir = sessions_dir();
    let mut entries: Vec<Entry> = vec![];

    if let Ok(rd) = fs::read_dir(&dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };

            let (meta_dt, last_event) = read_session_meta(&path);
            let dt = meta_dt
                .or_else(|| {
                    entry.metadata().ok().and_then(|m| m.modified().ok()).map(Into::into)
                })
                .unwrap_or_default();

            let (queue, champ_img, champ_pos, player_name, game_id, is_complete) = if let Some(ev) = last_event {
                let qid = ev["queueId"].as_u64().unwrap_or(0) as u32;
                let game_id = ev["gameId"].as_i64().filter(|&id| id != 0);
                let phase = ev["timer"]["phase"].as_str().unwrap_or("");
                let is_complete = matches!(phase, "GAME_STARTING" | "FINALIZATION");
                let local_cell = ev["localPlayerCellId"].as_u64().unwrap_or(0) as u32;

                let local_player = [&ev["myTeam"], &ev["theirTeam"]]
                    .iter()
                    .filter_map(|t| t.as_array())
                    .flatten()
                    .find(|p| p["cellId"].as_u64().unwrap_or(0) as u32 == local_cell);

                let (champ_img, champ_pos, player_name) = if let Some(p) = local_player {
                    let cid = p["championId"].as_u64().unwrap_or(0) as u32;
                    let pos = p["assignedPosition"].as_str().unwrap_or("");
                    let champ = if cid > 0 { champ_name(cid) } else { "" };
                    let img = champ_img_url(cid)
                        .map(|url| {
                            format!(r#"<img class="card-portrait" src="{url}" alt="{}">"#, esc(champ))
                        })
                        .unwrap_or_else(|| r#"<span class="card-portrait-ph"></span>"#.into());
                    let label = if !champ.is_empty() && !pos.is_empty() {
                        format!("{} · {}", esc(champ), pos_abbr(&pos.to_lowercase()))
                    } else if !champ.is_empty() {
                        esc(champ)
                    } else {
                        String::new()
                    };
                    let pname = p["gameName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| p["displayName"].as_str().filter(|s| !s.is_empty()))
                        .or_else(|| p["summonerName"].as_str().filter(|s| !s.is_empty()))
                        .unwrap_or("")
                        .to_string();
                    (img, label, pname)
                } else {
                    (r#"<span class="card-portrait-ph"></span>"#.into(), String::new(), String::new())
                };

                (queue_name(qid), champ_img, champ_pos, player_name, game_id, is_complete)
            } else {
                ("Custom", r#"<span class="card-portrait-ph"></span>"#.into(), String::new(), String::new(), None, true)
            };

            entries.push(Entry { dt, name, queue, champ_img, champ_pos, player_name, game_id, is_complete });
        }
    }

    entries.sort_by(|a, b| b.dt.cmp(&a.dt));

    let subtitle = match entries.len() {
        0 => "No drafts recorded yet".to_string(),
        1 => "1 draft recorded".to_string(),
        n => format!("{n} drafts recorded"),
    };

    let cards: String = entries
        .iter()
        .map(|e| {
            let date = e.dt.format("%Y-%m-%d %H:%M UTC").to_string();
            let meta = match e.game_id {
                Some(id) => format!("#{id} · {date}"),
                None => date,
            };
            let player_html = if !e.player_name.is_empty() {
                format!(r#"<span class="card-player">{}</span>"#, esc(&e.player_name))
            } else {
                String::new()
            };
            let aborted_html = if !e.is_complete {
                r#"<span class="card-aborted">Aborted</span>"#
            } else {
                ""
            };
            format!(
                r#"<a href="/session/{name}" class="card">{champ_img}<div class="card-info"><span class="card-queue">{queue}{aborted_html}</span><span class="card-champ">{champ_pos}</span><span class="card-meta">{meta}</span></div>{player_html}</a>"#,
                name = esc(&e.name),
                champ_img = e.champ_img,
                queue = esc(e.queue),
                aborted_html = aborted_html,
                champ_pos = e.champ_pos,
                meta = esc(&meta),
            )
        })
        .collect();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>DraftWatch</title>
<style>{LIST_CSS}</style></head>
<body>
<header><div class="header-inner"><h1>DraftWatch</h1><p class="sub">{sub}</p></div></header>
<main>{content}</main>
</body></html>"#,
        LIST_CSS = LIST_CSS,
        sub = esc(&subtitle),
        content = if cards.is_empty() {
            r#"<p class="empty">No drafts recorded yet.</p>"#.into()
        } else {
            cards
        },
    )
}

// ── Session detail page ───────────────────────────────────────────────────────

struct Player {
    cell_id: u32,
    champion_id: u32,
    position: String,
    display_name: String,
    team_id: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum SwapKind {
    PickOrder,
    Position,
}

struct SwapEvent {
    from_pos: String,
    to_pos: String,
    after_action_id: Option<u32>, // None = before any ban/pick; Some(id) = after that action
    pick_number: Option<u8>,      // overall draft pick slot (1st, 2nd, … 10th)
    kind: SwapKind,
    player_name: String, // name of the player at this cellId when the swap was detected
}

struct Action {
    id: u32,
    kind: String, // "ban" | "pick" — ten_bans_reveal is filtered out
    actor_cell_id: u32,
    champion_id: u32,
}

// Builds cellId → assignedPosition from both teams. Position changes between snapshots
// indicate a completed pick-order or position swap (see pickOrderSwaps / positionSwaps).
fn event_positions(ev: &Value) -> HashMap<u32, String> {
    [&ev["myTeam"], &ev["theirTeam"]]
        .iter()
        .filter_map(|t| t.as_array())
        .flatten()
        .filter_map(|p| {
            let cell_id = p["cellId"].as_u64()? as u32;
            let pos = p["assignedPosition"].as_str()?.to_string();
            if pos.is_empty() { return None; }
            Some((cell_id, pos))
        })
        .collect()
}

// Returns the highest completed ban/pick action ID at this snapshot. Used to anchor swap
// events to the correct position in the timeline. Intentionally excludes ten_bans_reveal
// (id ≈ 104) which has a non-sequential ID and would push swaps past all pick actions.
fn highest_completed_action(ev: &Value) -> Option<u32> {
    ev["actions"]
        .as_array()?
        .iter()
        .filter_map(|g| g.as_array())
        .flatten()
        .filter(|a| a["completed"].as_bool().unwrap_or(false))
        .filter(|a| matches!(a["type"].as_str(), Some("ban") | Some("pick")))
        .filter_map(|a| a["id"].as_u64())
        .map(|id| id as u32)
        .max()
}

fn ordinal(n: u8) -> String {
    let suffix = match n % 10 {
        1 if n % 100 != 11 => "st",
        2 if n % 100 != 12 => "nd",
        3 if n % 100 != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

// Returns cellId → overall draft pick slot (1-based), derived from pick action ordering.
fn event_pick_numbers(ev: &Value) -> HashMap<u32, u8> {
    let mut picks: Vec<(u32, u32)> = ev["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| g.as_array())
        .flatten()
        .filter(|a| a["type"].as_str() == Some("pick"))
        .filter_map(|a| {
            Some((a["id"].as_u64()? as u32, a["actorCellId"].as_u64()? as u32))
        })
        .collect();
    picks.sort_by_key(|(id, _)| *id);
    picks.into_iter().enumerate()
        .map(|(i, (_, cell_id))| (cell_id, (i + 1) as u8))
        .collect()
}

fn render_session(filename: &str) -> String {
    let path = sessions_dir().join(filename);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return error_page(&format!("Cannot read session: {e}")),
    };

    let mut last_event: Option<Value> = None;
    let mut first_ts: Option<String> = None;
    let mut pos_state: HashMap<u32, String> = HashMap::new();
    let mut swap_events: Vec<SwapEvent> = Vec::new();
    let mut last_busy_kind: Option<SwapKind> = None;
    // Maps action id → (player_name, position) captured the first time the action appears completed.
    // The LCU doesn't update actorCellId retroactively after pick-order swaps, so we must record
    // who occupied each cellId at the moment each action first became completed.
    let mut completed_action_actors: HashMap<u32, (String, String)> = HashMap::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            if first_ts.is_none() {
                first_ts = record["ts"].as_str().map(str::to_string);
            }
            if let Some(ev) = record.get("event").cloned() {
                // Detect which swap type is in-flight before positions change.
                // pickOrderSwaps / positionSwaps go BUSY one or more snapshots before
                // assignedPosition updates; BUSY resolves at the same snapshot the
                // position change appears, so we track the last-seen BUSY type.
                let has_busy = |field: &str| {
                    ev[field].as_array()
                        .map(|a| a.iter().any(|s| s["state"].as_str() == Some("BUSY")))
                        .unwrap_or(false)
                };
                if has_busy("pickOrderSwaps") {
                    last_busy_kind = Some(SwapKind::PickOrder);
                } else if has_busy("positionSwaps") {
                    last_busy_kind = Some(SwapKind::Position);
                }

                // Build cellId → (name, assignedPosition) for this snapshot.
                let cell_info: HashMap<u32, (String, String)> = [&ev["myTeam"], &ev["theirTeam"]]
                    .iter()
                    .filter_map(|t| t.as_array())
                    .flatten()
                    .filter_map(|p| {
                        let cid = p["cellId"].as_u64()? as u32;
                        let name = p["gameName"].as_str().filter(|s| !s.is_empty())
                            .or_else(|| p["displayName"].as_str().filter(|s| !s.is_empty()))
                            .or_else(|| p["summonerName"].as_str().filter(|s| !s.is_empty()))
                            .unwrap_or("").to_string();
                        let pos = p["assignedPosition"].as_str().unwrap_or("").to_string();
                        Some((cid, (name, pos)))
                    })
                    .collect();

                let new_positions = event_positions(&ev);
                let new_pick_nums = event_pick_numbers(&ev);
                let after = highest_completed_action(&ev);
                // Each change in assignedPosition is one half of a swap (two players
                // always swap simultaneously, so each completed swap emits two SwapEvents
                // that are later paired in render_swap_group).
                for (cell_id, new_pos) in &new_positions {
                    if let Some(old_pos) = pos_state.get(cell_id) {
                        if old_pos != new_pos {
                            let pname = cell_info.get(cell_id)
                                .map(|(n, _)| n.clone())
                                .unwrap_or_default();
                            swap_events.push(SwapEvent {
                                from_pos: old_pos.clone(),
                                to_pos: new_pos.clone(),
                                after_action_id: after,
                                pick_number: new_pick_nums.get(cell_id).copied(),
                                kind: last_busy_kind.unwrap_or(SwapKind::PickOrder),
                                player_name: pname,
                            });
                        }
                    }
                }
                if !new_positions.is_empty() && new_positions != pos_state {
                    last_busy_kind = None;
                }
                pos_state = new_positions;

                // Record which player was at each actorCellId the first time each action
                // appears completed. Later snapshots may have different players at the same
                // cellId after a pick-order swap, so .entry().or_insert() keeps the earliest.
                for group in ev["actions"].as_array().into_iter().flatten() {
                    if let Some(acts) = group.as_array() {
                        for a in acts {
                            if !a["completed"].as_bool().unwrap_or(false) { continue; }
                            let Some(action_id) = a["id"].as_u64().map(|id| id as u32) else { continue };
                            let Some(actor_cell_id) = a["actorCellId"].as_u64().map(|id| id as u32) else { continue };
                            completed_action_actors.entry(action_id).or_insert_with(|| {
                                cell_info.get(&actor_cell_id).cloned().unwrap_or_default()
                            });
                        }
                    }
                }

                last_event = Some(ev);
            }
        }
    }

    match last_event {
        Some(ev) => render_draft(filename, first_ts.as_deref(), &ev, &swap_events, &completed_action_actors),
        None => error_page("No valid events found in session."),
    }
}

fn render_draft(filename: &str, first_ts: Option<&str>, event: &Value, swap_events: &[SwapEvent], completed_action_actors: &HashMap<u32, (String, String)>) -> String {
    let game_id = event["gameId"].as_i64().filter(|&id| id != 0);
    let phase = event["timer"]["phase"].as_str().unwrap_or("");
    let is_complete = matches!(phase, "GAME_STARTING" | "FINALIZATION");
    let local_cell = event["localPlayerCellId"].as_u64().unwrap_or(0) as u32;

    // Collect all players from both teams
    let mut players: Vec<Player> = vec![];
    for (source, fallback_team) in [(&event["myTeam"], 100u32), (&event["theirTeam"], 200u32)] {
        if let Some(arr) = source.as_array() {
            for p in arr {
                players.push(Player {
                    cell_id: p["cellId"].as_u64().unwrap_or(0) as u32,
                    champion_id: p["championId"].as_u64().unwrap_or(0) as u32,
                    position: p["assignedPosition"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase(),
                    display_name: p["gameName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| p["displayName"].as_str().filter(|s| !s.is_empty()))
                        .or_else(|| p["summonerName"].as_str().filter(|s| !s.is_empty()))
                        .unwrap_or("")
                        .to_string(),
                    team_id: p["teamId"]
                        .as_u64()
                        .map(|t| t as u32)
                        .unwrap_or(fallback_team),
                });
            }
        }
    }

    // cellId → index in players vec, for timeline lookups
    let cell_idx: HashMap<u32, usize> = players
        .iter()
        .enumerate()
        .map(|(i, p)| (p.cell_id, i))
        .collect();

    // Local player's team id → determines which side is "you"
    let local_team = cell_idx
        .get(&local_cell)
        .and_then(|&i| players.get(i))
        .map(|p| p.team_id)
        .unwrap_or(100);

    // Local player's name — stable across snapshots and used to detect YOU in the timeline
    // after pick-order swaps have shifted which cellId the local player occupies.
    let local_name: String = cell_idx
        .get(&local_cell)
        .and_then(|&i| players.get(i))
        .map(|p| p.display_name.clone())
        .unwrap_or_default();

    // name → Player reference for champion lookup in swap rows.
    let name_to_player: HashMap<&str, &Player> = players
        .iter()
        .filter(|p| !p.display_name.is_empty())
        .map(|p| (p.display_name.as_str(), p))
        .collect();

    // Split and sort by position
    let mut blue: Vec<&Player> = players.iter().filter(|p| p.team_id == 100).collect();
    let mut red: Vec<&Player> = players.iter().filter(|p| p.team_id == 200).collect();
    blue.sort_by_key(|p| (pos_order(&p.position), p.cell_id));
    red.sort_by_key(|p| (pos_order(&p.position), p.cell_id));

    // Flatten actions, keep only completed ban/pick, sort by id
    let mut actions: Vec<Action> = vec![];
    if let Some(groups) = event["actions"].as_array() {
        for group in groups {
            if let Some(acts) = group.as_array() {
                for a in acts {
                    if !a["completed"].as_bool().unwrap_or(false) {
                        continue;
                    }
                    let kind = a["type"].as_str().unwrap_or("").to_string();
                    if kind != "ban" && kind != "pick" {
                        continue;
                    }
                    actions.push(Action {
                        id: a["id"].as_u64().unwrap_or(0) as u32,
                        kind,
                        actor_cell_id: a["actorCellId"].as_u64().unwrap_or(0) as u32,
                        champion_id: a["championId"].as_u64().unwrap_or(0) as u32,
                    });
                }
            }
        }
    }
    actions.sort_by_key(|a| a.id);

    // Champion trade detection: compare each player's first locked champion (from the
    // pick actions) against their final championId in the roster. A mismatch means a
    // trade occurred — the client's `trades` array only tracks slot availability/state
    // and does not record which champions were exchanged or when, so we infer it here.
    let pick_map: HashMap<u32, u32> = actions
        .iter()
        .filter(|a| a.kind == "pick" && a.champion_id > 0)
        .fold(HashMap::new(), |mut m, a| {
            m.entry(a.actor_cell_id).or_insert(a.champion_id);
            m
        });

    let mut trade_pairs: Vec<(u32, u32, u32, u32)> = vec![]; // (cell_a, champ_a_orig, cell_b, champ_b_orig)
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for p in &players {
        if p.champion_id == 0 || seen.contains(&p.cell_id) { continue; }
        let original = pick_map.get(&p.cell_id).copied().unwrap_or(0);
        if original > 0 && original != p.champion_id {
            if let Some(partner) = players.iter().find(|o| {
                o.cell_id != p.cell_id && pick_map.get(&o.cell_id).copied().unwrap_or(0) == p.champion_id
            }) {
                seen.insert(p.cell_id);
                seen.insert(partner.cell_id);
                let partner_orig = pick_map.get(&partner.cell_id).copied().unwrap_or(0);
                trade_pairs.push((p.cell_id, original, partner.cell_id, partner_orig));
            }
        }
    }

    let title = {
        let date_str = first_ts
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| {
                let utc: DateTime<chrono::Utc> = dt.into();
                utc.format("%Y-%m-%d %H:%M UTC").to_string()
            })
            .unwrap_or_else(|| filename.to_string());
        match game_id {
            Some(id) => format!("Game #{id} — {date_str}"),
            None => date_str,
        }
    };

    let blue_label = if local_team == 100 { "Blue Team (You)" } else { "Blue Team" };
    let red_label = if local_team == 200 { "Red Team (You)" } else { "Red Team" };

    let blue_html = render_roster(&blue, local_cell);
    let red_html = render_roster(&red, local_cell);
    let bench_html = render_bench(event);
    let timeline_html = render_timeline(&actions, &players, &cell_idx, local_cell, &local_name, &name_to_player, &trade_pairs, swap_events, completed_action_actors);

    let abort_notice = if !is_complete {
        let reason = match phase {
            "PLANNING" => "session ended during planning — no picks or bans were made",
            "BAN_PICK" => "session ended mid-draft — bans and picks are incomplete",
            _ => "session ended before the draft completed",
        };
        format!(r#"<div class="abort-notice">Draft aborted — {reason}</div>"#)
    } else {
        String::new()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>Draft — {title_esc}</title>
<style>{DETAIL_CSS}</style></head>
<body>
<header>
  <a class="back" href="/">&#8592; History</a>
  <h1>{title_esc}</h1>
</header>
<main>
  {abort_notice}
  <div class="teams">
    <div class="team blue-side">
      <h2 class="team-title blue">{blue_label}</h2>
      {blue_html}
    </div>
    <div class="team red-side">
      <h2 class="team-title red">{red_label}</h2>
      {red_html}
    </div>
  </div>
  {bench_html}
  <div class="tl-wrap">
    <h2 class="section-title">Draft Order</h2>
    <ol class="tl">{timeline_html}</ol>
  </div>
</main>
</body></html>"#,
        title_esc = esc(&title),
        DETAIL_CSS = DETAIL_CSS,
        abort_notice = abort_notice,
        blue_label = esc(blue_label),
        red_label = esc(red_label),
        blue_html = blue_html,
        red_html = red_html,
        bench_html = bench_html,
        timeline_html = timeline_html,
    )
}

fn render_bench(event: &Value) -> String {
    if !event["benchEnabled"].as_bool().unwrap_or(false) {
        return String::new();
    }
    let champs = match event["benchChampions"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return String::new(),
    };
    let rerolls = event["rerollsRemaining"].as_u64().unwrap_or(0);
    let rerolls_html = format!(
        r#"<span class="bench-rerolls">{rerolls} reroll{s} remaining</span>"#,
        s = if rerolls == 1 { "" } else { "s" },
    );
    let items: String = champs.iter().map(|c| {
        let id = c["championId"].as_u64().unwrap_or(0) as u32;
        let name = if id > 0 { champ_name(id) } else { "Unknown" };
        let img = champ_img_url(id)
            .map(|url| format!(r#"<img class="bench-portrait" src="{url}" alt="{}" loading="lazy">"#, esc(name)))
            .unwrap_or_else(|| r#"<span class="bench-portrait-ph"></span>"#.into());
        format!(r#"<div class="bench-champ">{img}<span class="bench-name">{}</span></div>"#, esc(name))
    }).collect();
    format!(
        r#"<div class="bench-wrap"><h2 class="section-title">Bench <span class="bench-rerolls-wrap">{rerolls_html}</span></h2><div class="bench-row">{items}</div></div>"#,
    )
}

fn render_roster(players: &[&Player], local_cell: u32) -> String {
    if players.is_empty() {
        return r#"<p class="empty-team">No data</p>"#.into();
    }
    players
        .iter()
        .map(|p| {
            let pos = pos_abbr(&p.position);
            let champ = if p.champion_id > 0 { champ_name(p.champion_id) } else { "—" };
            let img_html = champ_img_url(p.champion_id)
                .map(|url| format!(r#"<img class="portrait" src="{url}" alt="{}">"#, esc(champ)))
                .unwrap_or_else(|| r#"<span class="portrait-ph"></span>"#.into());
            let name_html = if !p.display_name.is_empty() {
                format!(r#"<span class="summoner">{}</span>"#, esc(&p.display_name))
            } else {
                String::new()
            };
            let you = if p.cell_id == local_cell {
                r#"<span class="you">YOU</span>"#
            } else {
                r#"<span class="you" style="visibility:hidden">YOU</span>"#
            };
            format!(
                r#"<div class="player">{img_html}<span class="pos">{pos}</span><span class="champ">{champ}</span><span class="player-right">{name_html}{you}</span></div>"#,
                pos = pos,
                champ = esc(champ),
            )
        })
        .collect()
}


fn swap_side(s: &SwapEvent, name_to_player: &HashMap<&str, &Player>) -> String {
    let champ_id = name_to_player.get(s.player_name.as_str())
        .map(|p| p.champion_id)
        .unwrap_or(0);
    let champ = if champ_id > 0 { champ_name(champ_id) } else { "" };
    let img = champ_img_url(champ_id)
        .map(|url| format!(r#"<img class="tl-portrait" src="{url}" alt="{}" loading="lazy">"#, esc(champ)))
        .unwrap_or_else(|| r#"<span class="tl-portrait-ph"></span>"#.into());
    let pick = s.pick_number
        .map(|n| ordinal(n))
        .unwrap_or_else(|| "—".into());
    let name = if !s.player_name.is_empty() {
        format!(r#"<span class="actor">{}</span>"#, esc(&s.player_name))
    } else {
        String::new()
    };
    let label = format!(
        r#"<span class="trade-label"><span class="champ">{pick}</span>{name}</span>"#,
    );
    format!("{img}{label}")
}

fn render_swap_row(s: &SwapEvent, name_to_player: &HashMap<&str, &Player>, local_name: &str) -> String {
    let you = if !local_name.is_empty() && s.player_name == local_name {
        r#"<span class="you">YOU</span>"#
    } else {
        r#"<span class="you" style="visibility:hidden">YOU</span>"#
    };
    let side = swap_side(s, name_to_player);
    let (cls, dot_cls, sub) = swap_kind_attrs(s.kind);
    format!(
        r#"<li class="action {cls}"><span class="n"></span><span class="dot {dot_cls}"></span><span class="tb">SWAP</span><span class="swap-sub">{sub}</span><div class="trade-inline">{side}</div>{you}</li>"#,
    )
}

fn render_swap_pair_row(s1: &SwapEvent, s2: &SwapEvent, name_to_player: &HashMap<&str, &Player>, local_name: &str) -> String {
    let involved = !local_name.is_empty()
        && (s1.player_name == local_name || s2.player_name == local_name);
    let you = if involved {
        r#"<span class="you">YOU</span>"#
    } else {
        r#"<span class="you" style="visibility:hidden">YOU</span>"#
    };
    let side_a = swap_side(s1, name_to_player);
    let side_b = swap_side(s2, name_to_player);
    let (cls, dot_cls, sub) = swap_kind_attrs(s1.kind);
    format!(
        r#"<li class="action {cls}"><span class="n"></span><span class="dot {dot_cls}"></span><span class="tb">SWAP</span><span class="swap-sub">{sub}</span><div class="trade-inline">{side_a}<span class="trade-inline-arrow">&#8596;</span>{side_b}</div>{you}</li>"#,
    )
}

// Returns (li-class, dot-class, sub-label) — the "SWAP" prefix is rendered separately.
fn swap_kind_attrs(kind: SwapKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        SwapKind::PickOrder => ("swap-order", "order-dot", "PICK"),
        SwapKind::Position  => ("swap-role",  "role-dot",  "ROLE"),
    }
}

fn render_swap_group(swaps: &[&SwapEvent], name_to_player: &HashMap<&str, &Player>, local_name: &str) -> String {
    let mut used = vec![false; swaps.len()];
    let mut html = String::new();
    for i in 0..swaps.len() {
        if used[i] { continue; }
        // Find a mirror partner: the other side of the same 2-way role swap.
        let partner = (i + 1..swaps.len()).find(|&j| {
            !used[j]
                && swaps[j].from_pos == swaps[i].to_pos
                && swaps[j].to_pos == swaps[i].from_pos
        });
        if let Some(j) = partner {
            used[i] = true;
            used[j] = true;
            html.push_str(&render_swap_pair_row(swaps[i], swaps[j], name_to_player, local_name));
        } else {
            used[i] = true;
            html.push_str(&render_swap_row(swaps[i], name_to_player, local_name));
        }
    }
    html
}

fn render_timeline(
    actions: &[Action],
    players: &[Player],
    cell_idx: &HashMap<u32, usize>,
    local_cell: u32,
    local_name: &str,
    name_to_player: &HashMap<&str, &Player>,
    trade_pairs: &[(u32, u32, u32, u32)],
    swap_events: &[SwapEvent],
    completed_action_actors: &HashMap<u32, (String, String)>,
) -> String {
    let mut html = String::new();
    let cell_to_player: HashMap<u32, &Player> = players.iter().map(|p| (p.cell_id, p)).collect();

    // Partition swaps: those before any ban/pick go at the top under "Pick Swaps";
    // others are keyed by the action after which they occurred and inserted inline.
    // If after_action_id points to an action not in the rendered set (e.g. ten_bans_reveal),
    // remap to the largest rendered action ID that precedes it.
    let rendered_ids: std::collections::BTreeSet<u32> = actions.iter().map(|a| a.id).collect();
    let resolve_action_id = |id: u32| -> Option<u32> {
        rendered_ids.range(..=id).next_back().copied()
    };

    let mut pre_swaps: Vec<&SwapEvent> = Vec::new();
    let mut swaps_after: HashMap<u32, Vec<&SwapEvent>> = HashMap::new();
    for s in swap_events {
        match s.after_action_id {
            None => pre_swaps.push(s),
            Some(id) => match resolve_action_id(id) {
                Some(resolved) => swaps_after.entry(resolved).or_default().push(s),
                None => pre_swaps.push(s),
            },
        }
    }
    if !pre_swaps.is_empty() {
        html.push_str(r#"<li class="phase-div">Swaps</li>"#);
        html.push_str(&render_swap_group(&pre_swaps, name_to_player, local_name));
    }

    // Build map: action_id → trades to insert after it.
    // A trade inserts after the later of the two players' pick action IDs.
    let last_pick_id: HashMap<u32, u32> = actions
        .iter()
        .filter(|a| a.kind == "pick" && a.champion_id > 0)
        .fold(HashMap::new(), |mut m, a| {
            let e = m.entry(a.actor_cell_id).or_insert(a.id);
            if a.id > *e { *e = a.id; }
            m
        });
    let mut trades_after: HashMap<u32, Vec<(u32, u32, u32, u32)>> = HashMap::new();
    for &tp in trade_pairs {
        let (cell_a, _, cell_b, _) = tp;
        let id_a = last_pick_id.get(&cell_a).copied().unwrap_or(0);
        let id_b = last_pick_id.get(&cell_b).copied().unwrap_or(0);
        trades_after.entry(id_a.max(id_b)).or_default().push(tp);
    }

    let mut prev_kind = "";
    let mut ban_phase = 0u32;
    let mut pick_phase = 0u32;

    for (i, a) in actions.iter().enumerate() {
        if a.kind.as_str() != prev_kind {
            let label = if a.kind == "ban" {
                ban_phase += 1;
                if ban_phase == 1 { "Bans" } else { "Bans — Phase 2" }
            } else {
                pick_phase += 1;
                if pick_phase == 1 { "Picks" } else { "Picks — Phase 2" }
            };
            html.push_str(&format!(r#"<li class="phase-div">{label}</li>"#));
            prev_kind = a.kind.as_str();
        }

        let n = i + 1;
        // Use the name/position captured when this action first appeared completed.
        // Falling back to the final-event cell mapping handles actions not yet seen
        // (shouldn't happen in practice since we scan all snapshots).
        let (actor_name, actor_pos): (&str, &str) = completed_action_actors
            .get(&a.id)
            .map(|(n, p)| (n.as_str(), p.as_str()))
            .unwrap_or_else(|| {
                cell_idx.get(&a.actor_cell_id)
                    .and_then(|&i| players.get(i))
                    .map(|p| (p.display_name.as_str(), p.position.as_str()))
                    .unwrap_or(("", ""))
            });
        let team_cls = name_to_player.get(actor_name)
            .map(|p| if p.team_id == 100 { "blue" } else { "red" })
            .unwrap_or(if a.actor_cell_id < 5 { "blue" } else { "red" });
        let type_cls = if a.kind == "ban" { "ban" } else { "pick" };
        let type_label = if a.kind == "ban" { "BAN" } else { "PICK" };
        let champ = if a.champion_id > 0 { champ_name(a.champion_id) } else { "—" };
        let img_html = champ_img_url(a.champion_id)
            .map(|url| format!(r#"<img class="tl-portrait" src="{url}" alt="{}" loading="lazy">"#, esc(champ)))
            .unwrap_or_default();
        let actor_html = if !actor_pos.is_empty() || !actor_name.is_empty() {
            let pos = pos_abbr(actor_pos);
            let name_part = if !actor_name.is_empty() {
                format!(" · {}", esc(actor_name))
            } else {
                String::new()
            };
            format!(r#"<span class="actor">{pos}{name_part}</span>"#)
        } else {
            String::new()
        };
        let you = if !local_name.is_empty() && actor_name == local_name {
            r#"<span class="you">YOU</span>"#
        } else {
            r#"<span class="you" style="visibility:hidden">YOU</span>"#
        };
        html.push_str(&format!(
            r#"<li class="action {team_cls} {type_cls}"><span class="n">{n}</span><span class="dot"></span><span class="tb">{type_label}</span>{img_html}<span class="champ">{champ}</span>{actor_html}{you}</li>"#,
            team_cls = team_cls,
            type_cls = type_cls,
            n = n,
            type_label = type_label,
            img_html = img_html,
            champ = esc(champ),
        ));

        // Insert any trades that happened after this pick.
        if let Some(trades) = trades_after.get(&a.id) {
            for &(cell_a, champ_a_orig, cell_b, champ_b_orig) in trades {
                let pa = cell_to_player.get(&cell_a);
                let pb = cell_to_player.get(&cell_b);
                let tl_img = |cid: u32| {
                    let cn = if cid > 0 { champ_name(cid) } else { "—" };
                    champ_img_url(cid)
                        .map(|url| format!(r#"<img class="tl-portrait" src="{url}" alt="{}" loading="lazy">"#, esc(cn)))
                        .unwrap_or_default()
                };
                let side = |p: Option<&&Player>, orig: u32| {
                    let cn = if orig > 0 { champ_name(orig) } else { "—" };
                    let name = p.filter(|pl| !pl.display_name.is_empty())
                        .map(|pl| format!(r#" · <span class="actor">{}</span>"#, esc(&pl.display_name)))
                        .unwrap_or_default();
                    format!(r#"<span class="trade-label"><span class="champ">{}</span>{name}</span>"#, esc(cn))
                };
                let involved = pa.map(|p| p.cell_id) == Some(local_cell)
                    || pb.map(|p| p.cell_id) == Some(local_cell);
                let you = if involved {
                    r#"<span class="you">YOU</span>"#
                } else {
                    r#"<span class="you" style="visibility:hidden">YOU</span>"#
                };
                html.push_str(&format!(
                    r#"<li class="action trade"><span class="n"></span><span class="dot trade-dot"></span><span class="tb">TRADE</span><div class="trade-inline">{img_a}{side_a}<span class="trade-inline-arrow">&#8646;</span>{side_b}{img_b}</div>{you}</li>"#,
                    img_a = tl_img(champ_a_orig),
                    side_a = side(pa, champ_a_orig),
                    img_b = tl_img(champ_b_orig),
                    side_b = side(pb, champ_b_orig),
                ));
            }
        }

        // Insert any pick swaps that happened after this action.
        if let Some(swaps) = swaps_after.get(&a.id) {
            html.push_str(&render_swap_group(swaps, name_to_player, local_name));
        }
    }

    html
}


// ── Helpers ───────────────────────────────────────────────────────────────────

fn pos_order(pos: &str) -> u32 {
    match pos {
        "top" => 0,
        "jungle" => 1,
        "middle" | "mid" => 2,
        "bottom" | "bot" | "adc" => 3,
        "utility" | "support" | "sup" => 4,
        _ => 5,
    }
}

fn pos_abbr(pos: &str) -> &'static str {
    match pos {
        "top" => "TOP",
        "jungle" => "JGL",
        "middle" | "mid" => "MID",
        "bottom" | "bot" | "adc" => "BOT",
        "utility" | "support" | "sup" => "SUP",
        _ => "—",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn error_page(msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html><head><title>Error — DraftWatch</title>
<style>body{{font-family:sans-serif;background:#0a0e1a;color:#c4b998;padding:40px}}a{{color:#4a9fd4;text-decoration:none}}</style>
</head><body>
<a href="/">&#8592; History</a>
<p style="margin-top:16px;color:#d44a4a">{}</p>
</body></html>"#,
        esc(msg)
    )
}

// ── Styles ────────────────────────────────────────────────────────────────────

const LIST_CSS: &str = "
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:'Segoe UI',sans-serif;background:#0a0e1a;color:#c4b998;min-height:100vh}
header{border-bottom:1px solid #1a2a4a}
.header-inner{max-width:700px;margin:0 auto;padding:24px 24px}
h1{color:#c8aa6e;font-size:1.4em;margin-bottom:4px}
.sub{color:#555;font-size:.9em}
main{max-width:700px;margin:24px auto;padding:0 24px}
.card{display:flex;align-items:center;gap:14px;padding:12px 16px;margin-bottom:8px;background:#0d1526;border:1px solid #1a2a4a;border-radius:4px;text-decoration:none;color:inherit;transition:border-color .15s}
.card:hover{border-color:#4a9fd4}
.card-portrait{width:44px;height:44px;border-radius:4px;object-fit:cover;flex-shrink:0;background:#1a2a4a}
.card-portrait-ph{width:44px;height:44px;border-radius:4px;background:#1a2a4a;flex-shrink:0}
.card-info{display:flex;flex-direction:column;gap:3px;min-width:0;flex:1}
.card-queue{font-size:.68em;font-weight:700;text-transform:uppercase;letter-spacing:.08em;color:#c8aa6e}
.card-champ{font-weight:600;color:#e8e0d0;font-size:.95em}
.card-meta{font-size:.78em;color:#555}
.card-player{margin-left:auto;font-size:.8em;color:#777;align-self:flex-start;flex-shrink:0;padding-left:8px}
.card-aborted{font-size:.65em;font-weight:700;background:rgba(212,74,74,.15);color:#d44a4a;padding:1px 5px;border-radius:2px;margin-left:6px;vertical-align:middle}
.empty{color:#444;padding:32px 0}
";

const DETAIL_CSS: &str = "
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:'Segoe UI',sans-serif;background:#0a0e1a;color:#c4b998;min-height:100vh}
header{display:flex;align-items:center;gap:16px;padding:14px 24px;border-bottom:1px solid #1a2a4a}
.back{color:#4a9fd4;text-decoration:none;font-size:.9em;flex-shrink:0}
.back:hover{text-decoration:underline}
h1{color:#c8aa6e;font-size:1.05em}
main{max-width:960px;margin:0 auto;padding:24px}
.teams{display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-bottom:32px}
.team{padding:16px;border-radius:4px}
.blue-side{background:#0b1830;border:1px solid #1a4a8c}
.red-side{background:#180b0b;border:1px solid #8c1a1a}
.team-title{font-size:.75em;font-weight:700;text-transform:uppercase;letter-spacing:.1em;margin-bottom:12px}
.team-title.blue{color:#4a9fd4}
.team-title.red{color:#d44a4a}
.player{display:flex;align-items:center;gap:10px;padding:7px 0;border-bottom:1px solid rgba(255,255,255,.05)}
.player:last-child{border-bottom:none}
.pos{font-size:.65em;font-weight:700;color:#555;text-transform:uppercase;width:26px;flex-shrink:0}
.champ{font-weight:600;color:#e8e0d0;flex:1}
.player-right{margin-left:auto;display:flex;align-items:center;gap:6px}
.summoner{font-size:.8em;color:#666}
.you{font-size:.65em;font-weight:700;background:rgba(200,170,110,.15);color:#c8aa6e;padding:1px 6px;border-radius:2px;flex-shrink:0}
.empty-team{color:#444;font-size:.85em;padding:8px 0}
.tl-wrap{}
.section-title{font-size:.8em;font-weight:700;text-transform:uppercase;letter-spacing:.1em;color:#c8aa6e;margin-bottom:10px}
.tl{list-style:none}
.action{display:flex;align-items:center;gap:10px;padding:7px 12px;margin:2px 0;border-radius:3px}
.action .n{width:20px;text-align:right;color:#3a3a3a;font-size:.75em;flex-shrink:0}
.action .dot{width:7px;height:7px;border-radius:50%;flex-shrink:0}
.action.blue .dot{background:#4a9fd4}
.action.red .dot{background:#d44a4a}
.action .tb{width:30px;font-size:.65em;font-weight:700;text-transform:uppercase;color:#555;flex-shrink:0}
.action .champ{font-weight:600}
.action .actor{font-size:.8em;color:#555;flex:1}
.action.ban{background:rgba(255,255,255,.02)}
.action.ban .champ{color:#444;text-decoration:line-through}
.action.pick.blue{background:rgba(20,60,130,.25)}
.action.pick.red{background:rgba(130,20,20,.25)}
.action.pick .champ{color:#e8e0d0}
.phase-div{list-style:none;padding:12px 12px 4px;font-size:.7em;font-weight:700;text-transform:uppercase;letter-spacing:.1em;color:#c8aa6e;border-top:1px solid #1a2a4a;margin-top:6px}
.phase-div:first-child{border-top:none;margin-top:0;padding-top:0}
.portrait{width:36px;height:36px;border-radius:3px;object-fit:cover;flex-shrink:0;background:#1a2a4a}
.portrait-ph{width:36px;height:36px;border-radius:3px;background:#1a2a4a;flex-shrink:0}
.tl-portrait{width:24px;height:24px;border-radius:2px;object-fit:cover;flex-shrink:0;background:#1a2a4a}
.action.ban .tl-portrait{filter:grayscale(1) opacity(.35)}
.abort-notice{background:rgba(212,74,74,.08);border:1px solid rgba(212,74,74,.25);color:#c06060;padding:10px 16px;border-radius:4px;font-size:.85em;margin-bottom:20px}
.action.trade{background:rgba(200,170,110,.07)}
.action.swap-order{background:rgba(74,159,212,.07)}
.action.swap-role{background:rgba(140,100,220,.07)}
.trade-dot{background:#c8aa6e !important}
.order-dot{background:#4a9fd4 !important}
.role-dot{background:#8c64dc !important}
.action.trade .tb{color:#c8aa6e}
.action.swap-order .tb{color:#4a9fd4}
.action.swap-role .tb{color:#8c64dc}
.action.swap-order .trade-inline-arrow{color:#4a9fd4}
.action.swap-role .trade-inline-arrow{color:#8c64dc}
.swap-sub{font-size:.6em;font-weight:700;text-transform:uppercase;color:#555;margin-right:6px;flex-shrink:0;align-self:center}
.trade-label .actor{margin-left:4px}
.trade-inline{display:flex;align-items:center;gap:8px;flex:1;min-width:0}
.trade-label{display:flex;align-items:baseline;gap:4px;min-width:0;flex-shrink:1}
.trade-label .champ{font-weight:600;color:#e8e0d0;white-space:nowrap;flex-shrink:0}
.trade-label .actor{font-size:.8em;color:#555;white-space:nowrap;flex:none;overflow:hidden;text-overflow:ellipsis}
.trade-inline-arrow{color:#c8aa6e;font-size:1.1em;flex-shrink:0}
.bench-wrap{margin-bottom:32px}
.bench-rerolls-wrap{font-weight:400;text-transform:none;letter-spacing:0}
.bench-rerolls{font-size:.8em;color:#555}
.bench-row{display:flex;flex-wrap:wrap;gap:10px;margin-top:8px}
.bench-champ{display:flex;flex-direction:column;align-items:center;gap:4px;width:52px}
.bench-portrait{width:48px;height:48px;border-radius:3px;object-fit:cover;background:#1a2a4a}
.bench-portrait-ph{width:48px;height:48px;border-radius:3px;background:#1a2a4a;display:block}
.bench-name{font-size:.6em;color:#666;text-align:center;line-height:1.2;word-break:break-word}
";
