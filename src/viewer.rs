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

            let (queue, champ_img, champ_pos, player_name, game_id) = if let Some(ev) = last_event {
                let qid = ev["queueId"].as_u64().unwrap_or(0) as u32;
                let game_id = ev["gameId"].as_i64().filter(|&id| id != 0);
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

                (queue_name(qid), champ_img, champ_pos, player_name, game_id)
            } else {
                ("Custom", r#"<span class="card-portrait-ph"></span>"#.into(), String::new(), String::new(), None)
            };

            entries.push(Entry { dt, name, queue, champ_img, champ_pos, player_name, game_id });
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
            format!(
                r#"<a href="/session/{name}" class="card">{champ_img}<div class="card-info"><span class="card-queue">{queue}</span><span class="card-champ">{champ_pos}</span><span class="card-meta">{meta}</span></div>{player_html}</a>"#,
                name = esc(&e.name),
                champ_img = e.champ_img,
                queue = esc(e.queue),
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

struct Action {
    id: u32,
    kind: String, // "ban" | "pick"
    actor_cell_id: u32,
    champion_id: u32,
}

fn render_session(filename: &str) -> String {
    let path = sessions_dir().join(filename);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return error_page(&format!("Cannot read session: {e}")),
    };

    let mut last_event: Option<Value> = None;
    let mut first_ts: Option<String> = None;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            if first_ts.is_none() {
                first_ts = record["ts"].as_str().map(str::to_string);
            }
            if let Some(ev) = record.get("event").cloned() {
                last_event = Some(ev);
            }
        }
    }

    match last_event {
        Some(ev) => render_draft(filename, first_ts.as_deref(), &ev),
        None => error_page("No valid events found in session."),
    }
}

fn render_draft(filename: &str, first_ts: Option<&str>, event: &Value) -> String {
    let game_id = event["gameId"].as_i64().filter(|&id| id != 0);
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

    // Build a map of cell_id → first picked champion (the one they originally selected).
    // If a player's final champion differs, they traded with whoever originally picked their champ.
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
    let timeline_html = render_timeline(&actions, &players, &cell_idx, local_cell);
    let trades_section = render_trades(&trade_pairs, &players);

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
  {trades_section}
  <div class="tl-wrap">
    <h2 class="section-title">Draft Order</h2>
    <ol class="tl">{timeline_html}</ol>
  </div>
</main>
</body></html>"#,
        title_esc = esc(&title),
        DETAIL_CSS = DETAIL_CSS,
        blue_label = esc(blue_label),
        red_label = esc(red_label),
        blue_html = blue_html,
        red_html = red_html,
        trades_section = trades_section,
        timeline_html = timeline_html,
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

fn render_timeline(
    actions: &[Action],
    players: &[Player],
    cell_idx: &HashMap<u32, usize>,
    local_cell: u32,
) -> String {
    let mut html = String::new();
    let mut prev_kind = "";
    let mut ban_phase = 0u32;
    let mut pick_phase = 0u32;

    for (i, a) in actions.iter().enumerate() {
        // Insert a phase divider whenever the action type flips
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
        let player = cell_idx.get(&a.actor_cell_id).and_then(|&i| players.get(i));
        let team_cls = player
            .map(|p| if p.team_id == 100 { "blue" } else { "red" })
            .unwrap_or(if a.actor_cell_id < 5 { "blue" } else { "red" });
        let type_cls = if a.kind == "ban" { "ban" } else { "pick" };
        let type_label = if a.kind == "ban" { "BAN" } else { "PICK" };
        let champ = if a.champion_id > 0 { champ_name(a.champion_id) } else { "—" };
        let img_html = champ_img_url(a.champion_id)
            .map(|url| {
                format!(
                    r#"<img class="tl-portrait" src="{url}" alt="{}" loading="lazy">"#,
                    esc(champ)
                )
            })
            .unwrap_or_default();
        let actor_html = player
            .map(|p| {
                let pos = pos_abbr(&p.position);
                let name_part = if !p.display_name.is_empty() {
                    format!(" · {}", esc(&p.display_name))
                } else {
                    String::new()
                };
                format!(r#"<span class="actor">{pos}{name_part}</span>"#)
            })
            .unwrap_or_default();
        let you = if a.actor_cell_id == local_cell {
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
    }

    html
}

fn render_trades(pairs: &[(u32, u32, u32, u32)], players: &[Player]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let cell_to_player: HashMap<u32, &Player> =
        players.iter().map(|p| (p.cell_id, p)).collect();

    let rows: String = pairs
        .iter()
        .map(|&(cell_a, champ_a_orig, cell_b, champ_b_orig)| {
            let pa = cell_to_player.get(&cell_a);
            let pb = cell_to_player.get(&cell_b);
            let name_a = pa.map(|p| p.display_name.as_str()).unwrap_or("").to_string();
            let name_b = pb.map(|p| p.display_name.as_str()).unwrap_or("").to_string();

            let img = |cid: u32| -> String {
                let champ = if cid > 0 { champ_name(cid) } else { "—" };
                champ_img_url(cid)
                    .map(|url| format!(r#"<img class="trade-portrait" src="{url}" alt="{}">"#, esc(champ)))
                    .unwrap_or_else(|| r#"<span class="trade-portrait-ph"></span>"#.into())
            };

            let label = |name: &str, orig: u32, _final: u32| -> String {
                let champ_orig = if orig > 0 { champ_name(orig) } else { "—" };
                if name.is_empty() {
                    format!(r#"<span class="trade-champ">{}</span>"#, esc(champ_orig))
                } else {
                    format!(
                        r#"<span class="trade-champ">{}</span><span class="trade-name">{}</span>"#,
                        esc(champ_orig),
                        esc(name)
                    )
                }
            };

            format!(
                r#"<div class="trade-row">{img_a}<div class="trade-side">{label_a}</div><span class="trade-arrow">&#8646;</span><div class="trade-side">{label_b}</div>{img_b}</div>"#,
                img_a = img(champ_a_orig),
                label_a = label(&name_a, champ_a_orig, champ_b_orig),
                img_b = img(champ_b_orig),
                label_b = label(&name_b, champ_b_orig, champ_a_orig),
            )
        })
        .collect();

    format!(
        r#"<div class="tl-wrap"><h2 class="section-title">Trades</h2><div class="trades">{rows}</div></div>"#
    )
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
.action .champ{min-width:130px;font-weight:600}
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
.trades{display:flex;flex-direction:column;gap:0}
.trade-row{display:flex;align-items:center;gap:10px;padding:8px 0;border-bottom:1px solid rgba(255,255,255,.05)}
.trade-row:last-child{border-bottom:none}
.trade-portrait{width:32px;height:32px;border-radius:3px;object-fit:cover;flex-shrink:0;background:#1a2a4a}
.trade-portrait-ph{width:32px;height:32px;border-radius:3px;background:#1a2a4a;flex-shrink:0}
.trade-side{display:flex;flex-direction:column;gap:2px;min-width:100px}
.trade-champ{font-weight:600;color:#e8e0d0;font-size:.9em}
.trade-name{font-size:.75em;color:#666}
.trade-arrow{color:#c8aa6e;font-size:1.2em;flex-shrink:0}
";
