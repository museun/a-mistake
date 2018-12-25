use std::collections::HashMap;
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use log::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::util;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq)]
pub enum Error {
    Exists,
    Save,
    Load,
    RunYoutubeDl,
    GetAudio,
    InvalidInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub duration: u64,
    pub thumbnail: String,
    pub fulltitle: String,
    #[serde(rename = "_filename")]
    pub filename: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub time: u64,
    pub owner: u64,
    pub info: VideoInfo,
}

const CONTROL_FILE: &str = "song_requests.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Control(HashMap<String, Request>);

impl Control {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        if let Ok(mut fi) = fs::File::open(path) {
            let len = fi.metadata().ok().map(|m| m.len()).unwrap_or_default();
            let mut buf = String::with_capacity(len as usize);
            fi.read_to_string(&mut buf).map_err(|_| Error::Load)?;
            return serde_json::from_str(&buf).map_err(|_| Error::Load);
        }
        Ok(Control::default())
    }
}

impl std::ops::Deref for Control {
    type Target = HashMap<String, Request>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Control {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct Playlist {
    list: Vec<Request>,
    pos: usize,
}

#[allow(dead_code)]
impl Playlist {
    pub fn new(list: Vec<Request>, pos: usize) -> Self {
        Self { list, pos }
    }

    pub fn play(&mut self, id: u64) -> Option<&Request> {
        if id >= self.len() as u64 {
            return None;
        }

        self.pos = id as usize;
        self.list.get(self.pos)
    }

    pub fn next(&mut self) -> Option<&Request> {
        if self.pos + 1 == self.len() {
            self.pos = 0;
        } else {
            self.pos += 1;
        }
        self.list.get(self.pos)
    }

    pub fn prev(&mut self) -> Option<&Request> {
        if self.pos == 0 {
            self.pos = self.len().saturating_sub(1);
        } else {
            self.pos -= 1;
        }
        self.list.get(self.pos)
    }

    pub fn random(&mut self) -> Option<&Request> {
        self.pos = thread_rng().gen_range(0, self.len());
        self.list.get(self.pos)
    }

    pub fn current(&self) -> Option<&Request> {
        self.list.get(self.pos)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Request> {
        self.list.iter()
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub struct Cache {
    base: PathBuf,
    map: HashMap<String, Request>,
    pattern: regex::Regex,
}

#[allow(dead_code)]
impl Cache {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        if !base.exists() {
            fs::create_dir(&base).expect("create dir");
        }

        let mut control = Control::load(base.join(CONTROL_FILE)).expect("load control");
        let map = fs::read_dir(&base)
            .expect("dir to exist")
            .filter_map(|dir| dir.and_then(|dir| Ok(dir.path())).ok())
            .filter_map(|entry| {
                entry
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .map(|id| control.remove(&id).map(|info| (id, info))) // this only uses known files
            // XXX: do we delete the orphaned files?
            .filter_map(|info| info)
            .collect();

        let pattern = regex::Regex::new(
               r#"(:?(:?^(:?http?.*?youtu(:?\.be|be.com))(:?/|.*?v=))(?P<id>[A-Za-z0-9_-]{11}))|(?P<id2>^[A-Za-z0-9_-]{11}$)"#,
            ).unwrap();

        Self { base, map, pattern }
    }

    pub fn make_playlist(&self, pos: Option<usize>) -> Playlist {
        let mut list = self.map.values().cloned().collect::<Vec<_>>();
        list.sort_by_key(|r| (r.time, std::cmp::Reverse(r.time)));
        Playlist::new(list, pos.unwrap_or(0))
    }

    pub fn exists(&self, id: impl AsRef<str>) -> bool {
        self.map.contains_key(id.as_ref())
    }

    pub fn get(&self, id: impl AsRef<str>) -> Option<&Request> {
        self.map.get(id.as_ref())
    }

    pub fn random(&mut self) -> Option<Request> {
        let key = self.map.keys().choose(&mut thread_rng())?;
        self.map.get(key).cloned()
    }

    pub fn ids_iter(&mut self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    pub fn add(&mut self, user: u64, input: &str) -> Result<Request> {
        let id = self
            .pattern
            .captures(input)
            .and_then(|s| s.name("id"))
            .ok_or_else(|| Error::InvalidInput)?
            .as_str()
            .to_string();

        if self.map.contains_key(&id) {
            return Err(Error::Exists);
        }

        info!("downloading {}", id);

        let now = util::timestamp();
        let (size, info) = self.download_video(&id)?;
        let end = util::timestamp();

        let ts = util::readable_time(Duration::from_millis(end - now));
        info!("[{}] fetched: {} in {}", &id, util::format_size(size), ts);

        let req = Request {
            time: now,
            owner: user,
            info,
        };
        self.map.insert(id, req.clone());
        self.save().expect("save cache file");
        Ok(req)
    }

    fn download_video(&self, id: &str) -> Result<(u64, VideoInfo)> {
        let quality = find_best_audio(id).ok_or_else(|| {
            error!("cannot get quality fmt for {}", id);
            Error::GetAudio
        })?;

        let json = Command::new("youtube-dl")
            .arg("--print-json")
            .arg("--add-metadata")
            .arg("-f")
            .arg(format!("{}", quality))
            .arg(id)
            .arg("-o")
            .arg(format!("{}/%(id)s.%(ext)s", self.base.to_string_lossy()))
            .output()
            .map_err(|err| {
                error!("cannot run youtube-dl: {}", err);
                Error::RunYoutubeDl
            })?;

        let info: VideoInfo = serde_json::from_slice(&json.stdout).map_err(|err| {
            error!("cannot deserialize json: {}", err);
            Error::GetAudio
        })?;

        fs::metadata(&info.filename)
            .map(|fi| (fi.len(), info))
            .map_err(|err| {
                error!("could not find file on disk: {}", err);
                Error::GetAudio
            })
    }

    fn save(&self) -> Result<()> {
        let mut fi = fs::File::create(self.base.join(CONTROL_FILE)).map_err(|_| Error::Save)?;
        let s = serde_json::to_string_pretty(&self.map).map_err(|_| Error::Save)?;
        fi.write_all(s.as_bytes()).map_err(|_| Error::Save)?;
        Ok(())
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.save().expect("save");
    }
}

fn find_best_audio(id: &str) -> Option<u64> {
    String::from_utf8_lossy(
        &Command::new("youtube-dl")
            .arg("-F")
            .arg(id)
            .output()
            .ok()?
            .stdout,
    )
    .lines()
    .skip_while(|s| {
        s.chars()
            .next()
            .map(|c| !c.is_ascii_digit())
            .unwrap_or_else(|| true)
    })
    .take_while(|s| {
        s.split_whitespace()
            .nth(2)
            .map(|c| c.starts_with('a'))
            .unwrap_or_else(|| false)
    })
    .flat_map(|line| {
        std::iter::once(line.chars().fold(
            ((0, 0, None), vec![], vec![]),
            |((mut fmt, mut bitrate, mut codec), mut letters, mut digits), ch| {
                match ch {
                    ' ' if !digits.is_empty() && (fmt == 0 || bitrate == 0) && codec.is_some() => {
                        let digits = digits
                            .drain(..)
                            .fold(0, |a, c| a * 10 + u64::from((c as u8) - b'0'));

                        match (fmt, bitrate) {
                            (0, ..) => fmt = digits,
                            (.., 0) => bitrate = digits,
                            _ => unreachable!(),
                        }
                    }

                    ' ' if codec.is_none() && !letters.is_empty() => {
                        codec.replace(letters.drain(..).fold(String::new(), |mut a, c| {
                            a.push(c);
                            a
                        }));
                    }

                    ' ' => letters.clear(),
                    ch if letters.is_empty() && ch.is_ascii_digit() => digits.push(ch),
                    ch => letters.push(ch),
                };

                ((fmt, bitrate, codec), letters, digits)
            },
        ))
        .map(|(s, ..)| s)
    })
    .filter_map(|(fmt, bitrate, codec)| codec.and_then(|_| Some((fmt, bitrate))))
    .max_by_key(|(.., bitrate)| *bitrate)
    .map(|(fmt, ..)| fmt)
}
