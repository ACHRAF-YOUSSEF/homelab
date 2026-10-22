use crate::Error;
use serde::Serialize;
use serde_json::{Map, Number, Value, json};
use std::collections::BTreeMap;

const DOWNLOAD: &[&str] = &[
    "downloading",
    "metaDL",
    "forcedMetaDL",
    "forcedDL",
    "queuedDL",
    "stalledDL",
    "checkingDL",
    "pausedDL",
    "stoppedDL",
];
const SEED: &[&str] = &[
    "uploading",
    "forcedUP",
    "queuedUP",
    "stalledUP",
    "checkingUP",
];

fn number(value: Option<&Value>, fallback: u64) -> Number {
    value
        .and_then(Value::as_number)
        .cloned()
        .unwrap_or_else(|| fallback.into())
}

fn text(value: Option<&Value>, max: usize) -> Result<String, Error> {
    let s = value.and_then(Value::as_str).unwrap_or_default();
    if s.len() > max {
        return Err(Error::Limit);
    }
    Ok(s.to_owned())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Torrent {
    name: String,
    state: String,
    progress: Number,
    download_speed: Number,
    upload_speed: Number,
    eta: Number,
}

impl Default for Torrent {
    fn default() -> Self {
        Self {
            name: String::new(),
            state: String::new(),
            progress: 0.into(),
            download_speed: 0.into(),
            upload_speed: 0.into(),
            eta: 0.into(),
        }
    }
}

pub struct QbState {
    pub rid: u64,
    items: BTreeMap<String, Torrent>,
    download: Number,
    upload: Number,
}

impl Default for QbState {
    fn default() -> Self {
        Self {
            rid: 0,
            items: BTreeMap::new(),
            download: 0.into(),
            upload: 0.into(),
        }
    }
}

impl QbState {
    pub fn apply(&mut self, payload: Value, max_items: usize) -> Result<Value, Error> {
        let object = payload.as_object().ok_or(Error::InvalidData)?;
        let full = object
            .get("full_update")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if full {
            *self = Self::default();
        }
        if let Some(torrents) = object.get("torrents").filter(|v| !v.is_null()) {
            let torrents = torrents.as_object().ok_or(Error::InvalidData)?;
            if torrents.len() > max_items {
                return Err(Error::Limit);
            }
            for (id, patch) in torrents {
                if id.len() > 128 {
                    return Err(Error::Limit);
                }
                let Some(patch) = patch.as_object() else {
                    continue;
                };
                if !self.items.contains_key(id) && self.items.len() >= max_items {
                    return Err(Error::Limit);
                }
                let item = self.items.entry(id.clone()).or_default();
                if patch.contains_key("name") {
                    item.name = text(patch.get("name"), 4096)?;
                }
                if patch.contains_key("state") {
                    item.state = text(patch.get("state"), 128)?;
                }
                if patch.contains_key("progress") {
                    item.progress = number(patch.get("progress"), 0);
                }
                if patch.contains_key("dlspeed") {
                    item.download_speed = number(patch.get("dlspeed"), 0);
                }
                if patch.contains_key("upspeed") {
                    item.upload_speed = number(patch.get("upspeed"), 0);
                }
                if patch.contains_key("eta") {
                    item.eta = number(patch.get("eta"), 0);
                }
            }
        }
        if !full && let Some(removed) = object.get("torrents_removed").and_then(Value::as_array) {
            if removed.len() > max_items {
                return Err(Error::Limit);
            }
            for id in removed.iter().filter_map(Value::as_str) {
                self.items.remove(id);
            }
        }
        if let Some(server) = object.get("server_state").and_then(Value::as_object) {
            if server.contains_key("dl_info_speed") {
                self.download = number(server.get("dl_info_speed"), 0);
            }
            if server.contains_key("up_info_speed") {
                self.upload = number(server.get("up_info_speed"), 0);
            }
        }
        if let Some(rid) = object.get("rid").and_then(Value::as_u64) {
            self.rid = rid;
        }
        let mut torrents = Vec::with_capacity(self.items.len());
        let mut seeding = 0;
        for (id, item) in &self.items {
            if SEED.contains(&item.state.as_str()) {
                seeding += 1;
            }
            if DOWNLOAD.contains(&item.state.as_str()) {
                let mut torrent = serde_json::to_value(item).map_err(|_| Error::InvalidData)?;
                torrent
                    .as_object_mut()
                    .ok_or(Error::InvalidData)?
                    .insert("id".into(), Value::String(id.clone()));
                torrents.push(torrent);
            }
        }
        Ok(
            json!({"downloadSpeed": self.download, "uploadSpeed": self.upload, "seedingCount": seeding, "downloadingCount": torrents.len(), "torrents": torrents}),
        )
    }
}

pub fn normalize_sessions(payload: &Value, max: usize) -> Result<Value, Error> {
    let items = payload.as_array().ok_or(Error::InvalidData)?;
    if items.len() > max {
        return Err(Error::Limit);
    }
    let mut sessions = Vec::with_capacity(items.len());
    for item in items {
        let Some(now) = item.get("NowPlayingItem").and_then(Value::as_object) else {
            continue;
        };
        let empty = Map::new();
        let play = item
            .get("PlayState")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let id = item
            .get("Id")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .or_else(|| item.get("SessionId"));
        let method = text(play.get("PlayMethod"), 128)?;
        let method = if method.is_empty()
            && item
                .get("TranscodingInfo")
                .is_some_and(|v| !v.is_null() && v != &json!({}))
        {
            "Transcode".into()
        } else {
            method
        };
        let mut rate = number(play.get("PlaybackRate"), 1);
        if rate.as_f64() == Some(0.0) {
            rate = 1.into();
        }
        sessions.push(json!({
            "id": text(id, 4096)?, "userName": text(item.get("UserName"), 4096)?,
            "title": text(now.get("Name"), 4096)?, "seriesName": text(now.get("SeriesName"), 4096)?,
            "type": text(now.get("Type"), 128)?, "season": now.get("ParentIndexNumber").filter(|v| v.is_number()),
            "episode": now.get("IndexNumber").filter(|v| v.is_number()),
            "client": text(item.get("Client"), 4096)?, "deviceName": text(item.get("DeviceName"), 4096)?,
            "playMethod": method, "isPaused": play.get("IsPaused").and_then(Value::as_bool).unwrap_or(false),
            "positionTicks": number(play.get("PositionTicks"), 0), "durationTicks": number(now.get("RunTimeTicks"), 0),
            "playbackRate": rate,
        }));
    }
    Ok(json!({"sessions": sessions}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_incremental_removed_and_state_filters() {
        let mut state = QbState::default();
        let first = state.apply(json!({"rid":1,"full_update":true,"server_state":{"dl_info_speed":123,"up_info_speed":7},"torrents":{
            "a":{"name":"episode.mkv","state":"downloading","progress":0.5},"seed":{"state":"checkingUP"},"excluded":{"state":"stoppedUP"}
        }}), 10).unwrap();
        assert_eq!(first["downloadingCount"], 1);
        assert_eq!(first["seedingCount"], 1);
        let next = state.apply(json!({"rid":2,"server_state":{"up_info_speed":9},"torrents":{"a":{"state":"pausedDL"}}}),10).unwrap();
        assert_eq!(next["torrents"][0]["name"], "episode.mkv");
        assert_eq!(next["torrents"][0]["progress"], 0.5);
        assert_eq!(next["downloadSpeed"], 123);
        assert_eq!(next["uploadSpeed"], 9);
        let removed = state.apply(json!({"torrents_removed":["a"]}), 10).unwrap();
        assert_eq!(removed["downloadingCount"], 0);
        let full = state
            .apply(
                json!({"full_update":true,"torrents":{"b":{"state":"forcedMetaDL"}}}),
                10,
            )
            .unwrap();
        assert_eq!(full["uploadSpeed"], 0);
        assert_eq!(full["seedingCount"], 0);
        assert_eq!(full["downloadingCount"], 1);
    }

    #[test]
    fn normalized_sessions_exclude_idle_and_use_transcode_fallback() {
        let result = normalize_sessions(&json!([{"Id":"idle"},{"Id":"active","NowPlayingItem":{"Name":"Film","RunTimeTicks":1000},"PlayState":{"PositionTicks":20,"PlaybackRate":0},"TranscodingInfo":{"Bitrate":1}}]), 10).unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(result["sessions"][0]["playMethod"], "Transcode");
        assert_eq!(result["sessions"][0]["playbackRate"], 1);
        assert!(normalize_sessions(&json!([{}, {}]), 1).is_err());
    }

    #[test]
    fn sessions_require_an_array_but_allow_empty_and_idle_results() {
        for payload in [
            json!(null),
            json!({}),
            json!({"Items": []}),
            json!("unavailable"),
            json!(42),
            json!(false),
        ] {
            assert!(matches!(
                normalize_sessions(&payload, 10),
                Err(Error::InvalidData)
            ));
        }
        for payload in [json!([]), json!([{"Id": "idle"}])] {
            assert_eq!(
                normalize_sessions(&payload, 10).unwrap(),
                json!({"sessions": []})
            );
        }
    }

    #[test]
    fn incremental_state_cannot_grow_without_bound() {
        let mut state = QbState::default();
        state.apply(json!({"torrents":{"a":{}}}), 1).unwrap();
        assert!(state.apply(json!({"torrents":{"b":{}}}), 1).is_err());
        assert!(
            state
                .apply(json!({"torrents":{"a":{"name":"x".repeat(4097)}}}), 1)
                .is_err()
        );
    }
}
