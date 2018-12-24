use crate::{cache, mpv};
use std::io;

use log::*;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    MpvError(mpv::Error),
    IoError(io::Error),
    InvalidResponse(String),
    NotPlaying,
}

impl From<mpv::Error> for Error {
    fn from(err: mpv::Error) -> Self {
        Error::MpvError(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::IoError(err)
    }
}

pub struct Control {
    client: mpv::Client,
}

#[allow(dead_code)]
impl Control {
    pub fn new(client: mpv::Client) -> Self {
        Self { client }
    }

    pub fn play(&mut self, req: &cache::Request) -> Result<bool> {
        debug!("trying to play: #{}: {}", req.owner, req.info.fulltitle);
        self.stop()?;
        let cmd = mpv::Command::LoadFile(req.info.filename.clone());
        self.write_cmd(cmd)
    }

    pub fn stop(&mut self) -> Result<bool> {
        self.write_cmd(mpv::Command::Stop)
    }

    pub fn title(&mut self) -> Result<String> {
        match self.get("media-title") {
            Err(err) => {
                if let Error::InvalidResponse(s) = &err {
                    if s == "property unavailable" {
                        return Err(Error::NotPlaying);
                    }
                }
                Err(err)
            }
            other => other,
        }
    }

    pub fn filename(&mut self) -> Result<String> {
        match self.get("filename") {
            Err(err) => {
                if let Error::InvalidResponse(s) = &err {
                    if s == "property unavailable" {
                        return Err(Error::NotPlaying);
                    }
                }
                Err(err)
            }
            other => other,
        }
    }

    pub fn time(&mut self) -> Result<f64> {
        self.get("playback-time")
    }

    pub fn duration(&mut self) -> Result<f64> {
        self.get("duration")
    }

    pub fn check_playing(&mut self) -> bool {
        match self.title() {
            Err(Error::NotPlaying) | Err(..) => false,
            Ok(..) => true,
        }
    }

    pub fn wait_for_ready(&mut self) -> Result<()> {
        self.client
            .wait_for_event(mpv::Event::FileLoaded)
            .map_err(|e| e.into())
    }

    pub fn wait_for_end(&mut self) -> Result<()> {
        self.client
            .wait_for_event(mpv::Event::EndFile)
            .map_err(|e| e.into())
    }

    pub fn write_cmd(&mut self, cmd: mpv::Command) -> Result<bool> {
        self.client.write_ok(cmd).map_err(|e| e.into())
    }

    pub fn get<T>(&mut self, prop: &str) -> Result<T>
    where
        for<'de> T: serde::de::Deserialize<'de> + std::fmt::Debug,
    {
        let cmd = mpv::Command::get(prop);
        let resp = self.client.write_command(cmd)?;
        trace!("resp: {:?}", resp);
        Self::check_response(resp)
    }

    fn check_response<T>(resp: mpv::Response<T>) -> Result<T> {
        if resp.success() {
            Ok(resp.data.unwrap())
        } else {
            Err(Error::InvalidResponse(resp.error().into()))
        }
    }
}
