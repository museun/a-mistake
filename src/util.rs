use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use log::*;
use serde::Deserialize;

pub fn place_commas(n: u64) -> String {
    fn commas(n: u64, s: &mut String) {
        if n < 1000 {
            s.push_str(&n.to_string());
            return;
        }
        commas(n / 1000, s);
        s.push_str(&format!(",{:03}", n % 1000))
    }

    let len = count_digits(n);
    let mut buf = String::with_capacity((len + (len / 3 % 3)) as usize);
    commas(n, &mut buf);
    buf
}

fn count_digits(mut n: u64) -> u64 {
    if n == 0 {
        return 1;
    }

    let mut x = 0;
    while n > 0 {
        n /= 10;
        x += 1;
    }
    x
}

pub fn format_size(n: u64) -> String {
    const SIZES: [&str; 9] = ["B", "K", "M", "G", "T", "P", "E", "Z", "Y"]; // sure
    let mut order = 0;
    let mut size = n as f64;

    while size >= 1024.0 && order + 1 < SIZES.len() {
        order += 1;
        size /= 1024.0
    }

    format!("{:.2} {}", size, SIZES[order])
}

pub fn timestamp() -> u64 {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    ts.as_secs() * 1000 + u64::from(ts.subsec_nanos()) / 1_000_000
}

#[allow(dead_code)]
pub fn readable_timestamp(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

pub fn readable_time(dur: Duration) -> String {
    const TABLE: [(&str, u64); 3] = [
        ("hours", 3600), //
        ("minutes", 60), //
        ("seconds", 1),  //
    ];

    let mut time = vec![];
    let mut secs = dur.as_secs();

    for (name, d) in &TABLE {
        let div = secs / d;
        if div > 0 {
            time.push((name, div));
            secs -= d * div
        }
    }

    fn plural((s, n): (&str, u64)) -> String {
        format!("{} {}", n, if n > 1 { s } else { &s[..s.len() - 1] })
    }

    let mut list = time
        .into_iter()
        .map(|(s, n)| (*s, n))
        .filter(|&(.., n)| n > 0)
        .map(plural)
        .collect::<Vec<_>>();

    let len = list.len();
    if len > 1 {
        if len > 2 {
            for el in list.iter_mut().take(len - 2) {
                el.push(',')
            }
        }
        list.insert(len - 1, "and".into())
    }

    list.join(" ")
}

pub fn get_usernames(ids: impl IntoIterator<Item = u64>) -> Option<Vec<(u64, String)>> {
    const BASE_URL: &str = "https://api.twitch.tv/helix";

    let client_id = std::env::var("SHAKEN_TWITCH_CLIENT_ID").ok().or_else(|| {
        error!("SHAKEN_TWITCH_CLIENT_ID is not set");
        None
    })?;

    let set = ids.into_iter().collect::<HashSet<_>>();
    let ids = set.into_iter().fold(String::new(), |mut a, id| {
        a.push_str(&format!("id={}&", id));
        a
    });

    debug!("ids: {}", ids);
    if ids.is_empty() {
        return None;
    }

    let mut easy = curl::easy::Easy::new();
    let mut list = curl::easy::List::new();
    list.append(&format!("Client-ID: {}", client_id)).unwrap();
    easy.http_headers(list).unwrap();

    let mut body = vec![];
    let url = format!("{}/users?{}", BASE_URL, ids);
    easy.url(&url).ok()?;
    {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|data| {
                body.extend_from_slice(&data);
                Ok(data.len())
            })
            .map_err(|err| {
                warn!("could get user names from twitch: {}", err);
                err
            })
            .ok()?;

        transfer
            .perform()
            .map_err(|err| {
                warn!("could get user names from twitch: {}", err);
                err
            })
            .ok()?;
    }

    serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|val| val.get("data").and_then(|s| s.as_array()).cloned())
        .and_then(|array| {
            array
                .into_iter()
                .filter_map(|val| serde_json::from_value::<User>(val).ok())
                .map(|user| Some((user.id.parse::<u64>().ok()?, user.display_name)))
                .collect()
        })
}

#[derive(Deserialize, Debug)]
pub struct User {
    pub id: String,
    pub login: String,
    pub display_name: String,
}
