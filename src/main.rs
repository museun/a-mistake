#![feature(bind_by_move_pattern_guards)]
mod cache;
mod control;
mod irc;
mod mpv;
mod twitch;
mod util;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use chrono::prelude::*;
use log::*;
use simplelog::{Config, LevelFilter, TermLogger};

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Mpv(mpv::Error),
    Cache(cache::Error),
    Twitch(twitch::Error),
    EmptyPlaylist,
    NotPlaying,
}

impl From<mpv::Error> for Error {
    fn from(err: mpv::Error) -> Self {
        Error::Mpv(err)
    }
}

impl From<cache::Error> for Error {
    fn from(err: cache::Error) -> Self {
        Error::Cache(err)
    }
}

impl From<twitch::Error> for Error {
    fn from(err: twitch::Error) -> Self {
        Error::Twitch(err)
    }
}

fn new_client() -> mpv::Client {
    #[cfg(windows)]
    return mpv::Client::new(miow::pipe::connect("//./pipe/tmp/mpvsocket").unwrap());

    #[cfg(not(windows))]
    return mpv::Client::new(std::fs::File::open("tmp/mpvsocket").unwrap());
}

struct UserMap(HashMap<u64, String>);

impl UserMap {
    pub fn new() -> Self {
        Self { 0: HashMap::new() }
    }

    pub fn add_many(&mut self, ids: impl IntoIterator<Item = u64>) -> Option<()> {
        let iter = ids
            .into_iter()
            .map(|id| (id, self.0.contains_key(&id)))
            .filter(|(_, ok)| !*ok)
            .map(|(i, _)| i);

        util::get_usernames(iter)?
            .into_iter()
            .for_each(|(id, name)| {
                self.0.insert(id, name);
            });

        Some(())
    }

    pub fn get(&mut self, id: u64) -> Option<String> {
        if let Some(user) = self.0.get(&id) {
            return Some(user.clone()); // shitty
        }

        self.add_many([id].iter().cloned())?;
        Some(self.0[&id].clone()) // shitty
    }
}

type PlaylistRef = Arc<RwLock<cache::Playlist>>;

use std::rc::Rc;

struct Bot {
    cache: cache::Cache,
    playlist: PlaylistRef,
    control: control::Control,
    twitch: twitch::Client,
    user_map: UserMap,

    dirty: bool,
    paste: Option<Rc<String>>,
}

impl Bot {
    pub fn new(cache: cache::Cache, playlist: PlaylistRef) -> Result<Self> {
        Ok(Self {
            cache,
            playlist,
            control: control::Control::new(new_client()),
            twitch: twitch::Client::connect("museun", "shaken_bot")?,
            user_map: UserMap::new(),

            dirty: true,
            paste: None,
        })
    }

    pub fn start(mut self) -> Result<()> {
        use self::twitch::{Command, CommandKind::*};

        loop {
            let msg = self.twitch.next_message()?;
            let cmd = match Command::parse(&msg) {
                Some(cmd) => cmd,
                None => continue,
            };

            macro_rules! maybe {
                ($e:expr, $f:expr) => {
                    match $e {
                        Some(e) => e,
                        None => {
                            warn!("invalid result: {}", $f);
                            self.twitch.reply(cmd.target, $f)?;
                            continue;
                        },
                    }
                };
                ($e:expr, $f:expr, $($args:expr),*) => {
                    match $e {
                        Some(e) => e,
                        None => {
                            let s = format!($f, $($args),*);
                            self.twitch.reply(cmd.target, & s)?;
                            continue;
                        },
                    }
                };
            }

            match cmd.kind {
                Request { id, req } => {
                    for resp in self.try_song_request((id, req)).iter() {
                        self.dirty = true;
                        self.twitch.reply(cmd.target, &resp)?
                    }
                }

                Info | Skip | Random if !self.control.check_playing() => {
                    self.twitch.reply(cmd.target, "No song is playing")?
                }

                List => {
                    // don't report this
                    if let Some(link) = self.generate_list() {
                        self.twitch.reply(cmd.target, &link)?
                    }
                }

                Info => self.send_song_info(cmd.target)?,

                Play { pos } => {
                    let pos = maybe!(pos.parse::<u64>().ok(), "invalid number");
                    maybe!(self.play_song(pos), "could not play: {}", pos);
                    self.send_song_info(cmd.target)?
                }

                Skip => {
                    maybe!(self.skip_song(), "could not skip that song");
                    self.send_song_info(cmd.target)?
                }

                Random => {
                    maybe!(self.random_song(), "could not play a random song");
                    self.send_song_info(cmd.target)?
                }
            }
        }
    }

    fn send_song_info<'a>(&mut self, target: twitch::Target<'a>) -> Result<()> {
        for resp in self.get_song_info().iter().flat_map(|list| list.iter()) {
            self.twitch.reply(target, resp)?
        }
        Ok(())
    }

    fn try_song_request(&mut self, (id, req): (&str, &str)) -> Option<String> {
        let id = id.parse::<u64>().ok()?;
        let res = match self.cache.add(id, req) {
            Err(cache::Error::InvalidInput) => "cannot parse that input",
            Err(cache::Error::Exists) => "that request already exists",
            Err(err) => {
                error!(
                    "error trying to add '{}' from {} to the cache: {:?}",
                    req, id, err
                );
                "something went wrong with adding that"
            }
            Ok(res) => {
                let pos = { self.playlist.read().unwrap().pos() };
                let new_playlist = self.cache.make_playlist(Some(pos));
                std::mem::replace(&mut *self.playlist.write().unwrap(), new_playlist);
                let len = { self.playlist.read().unwrap().len() };

                let cache::VideoInfo { fulltitle, .. } = &res.info;
                return Some(format!(
                    "added song #{} -> {}",
                    util::place_commas(len as u64 - 1),
                    fulltitle
                ));
            }
        };

        Some(res).map(String::from)
    }

    fn generate_list(&mut self) -> Option<Rc<String>> {
        // go ahead and update the user map as eagerly as possible
        let list = self.playlist.read().unwrap();
        self.user_map
            .add_many(list.iter().map(|cache::Request { owner, .. }| *owner));

        // if the playlist hasn't changed, reuse old paste
        if !self.dirty && self.paste.is_some() {
            return self.paste.clone();
        }

        use std::borrow::Cow;
        let unknown = Cow::from("unknown");

        let mut out = vec![];
        for (i, req) in list.iter().enumerate() {
            let cache::Request {
                owner,
                time,
                info: cache::VideoInfo { id, fulltitle, .. },
            } = &req;

            let user = self
                .user_map
                .get(*owner)
                .and_then(|s| Some(Cow::from(s)))
                .unwrap_or_else(|| unknown.clone());

            let ts = Local.timestamp_millis(*time as i64);
            let s = format!(
                "#{}\t{}\nlink\thttps://www.youtube.com/watch?v={}\nfrom\t{} at {}\n\n", //
                i, fulltitle, id, user, ts
            );
            out.push(s);
        }

        macro_rules! check {
            ($e:expr) => {
                if let Err(err) = $e {
                    error!("error!: {:?}", err);
                    return None;
                }
            };
        }

        use curl::easy::{Easy, Form};
        let mut easy = Easy::new();
        check!(easy.url("http://ix.io"));

        let mut form = Form::new();
        check!(form
            .part("f:1")
            .contents(
                &out.iter()
                    .fold(String::new(), |mut a, c| {
                        a.push_str(&c);
                        a
                    })
                    .as_bytes()
            )
            .add());
        check!(easy.httppost(form));

        let mut data = vec![];
        {
            let mut transfer = easy.transfer();
            check!(transfer.write_function(|d| {
                data.extend_from_slice(&d);
                Ok(d.len())
            }));

            check!(transfer.perform());
        }

        self.dirty = false;
        let resp = String::from_utf8_lossy(&data);
        self.paste.replace(Rc::new(resp.into())); // TODO use a Cow here
        self.paste.clone()
    }

    fn get_song_info(&mut self) -> Option<Vec<String>> {
        let playlist = self.playlist.read().unwrap();
        let req = playlist.current()?;

        // XXX maybe get the timestamp here
        let mut out = vec![];
        out.push(format!(
            "“{}” - youtu.be/{}",
            req.info.fulltitle, req.info.id
        ));

        let time = util::readable_time(Duration::from_millis(util::timestamp() - req.time));
        let user = self
            .user_map
            .get(req.owner)
            .unwrap_or_else(|| "unknown".into());
        out.push(format!("requested by {}, {} ago", user, time));

        Some(out)
    }

    // TODO use Results here instead of Options
    fn random_song(&mut self) -> Option<bool> {
        let mut playlist = self.playlist.write().unwrap();
        self.control.play(&playlist.random().cloned()?).ok()
    }

    fn skip_song(&mut self) -> Option<bool> {
        let mut playlist = self.playlist.write().unwrap();
        self.control.play(&playlist.next().cloned()?).ok()
    }

    fn play_song(&mut self, id: u64) -> Option<bool> {
        let mut playlist = self.playlist.write().unwrap();
        self.control.play(&playlist.play(id).cloned()?).ok()
    }
}

fn main() {
    let _ = TermLogger::init(LevelFilter::Trace, Config::default());

    let mut cache = cache::Cache::new("foo");
    let mut control = control::Control::new(new_client());

    let pos = control
        .filename()
        .ok()
        .map(PathBuf::from)
        .and_then(|p| {
            p.file_stem()
                .and_then(|stem| stem.to_str())
                .map(|s| s.to_string())
        })
        .and_then(|name| cache.ids_iter().position(|id| *id == name));

    let playlist = Arc::new(RwLock::new(cache.make_playlist(pos)));

    {
        let playlist = Arc::clone(&playlist);
        thread::spawn(move || {
            if let Err(err) = Bot::new(cache, playlist).and_then(|bot| bot.start()) {
                error!("run into a error while running the bot: {:?}", err);
                std::process::exit(1); // just die
            }
        });
    }

    loop {
        match playlist.read().unwrap().current() {
            Some(current) => {
                control.play(current).unwrap();
            }
            None => warn!("no songs in the playlist"),
        }
        // wait for the file to start
        control.wait_for_ready().unwrap();

        // wait for the file to end
        control.wait_for_end().unwrap();
    }
}
