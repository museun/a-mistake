use std::collections::HashMap;
use std::fs::File;
use std::io::{self, prelude::*, BufRead, BufReader};

use indexmap::IndexSet;
use log::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::IoError(err)
    }
}

pub struct Client {
    reader: BufReader<File>,
    writer: File,

    events: IndexSet<Event>,
    buf: HashMap<u8, Value>, // XXX LRU eviction might be a good idea
}

impl Client {
    pub fn new(fi: File) -> Self {
        let writer = fi.try_clone().unwrap();
        let reader = BufReader::new(fi);
        Self {
            writer,
            reader,

            events: IndexSet::new(),
            buf: HashMap::new(),
        }
    }

    pub fn write_ok(&mut self, cmd: Command) -> Result<bool> {
        let resp = self.write_command::<bool>(cmd)?;
        Ok(resp.success())
    }

    pub fn write_command<T>(&mut self, cmd: Command) -> Result<Response<T>>
    where
        for<'de> T: serde::de::Deserialize<'de>,
    {
        let req = Request::new(cmd);
        let json = serde_json::to_string(&req)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "failed to serialize json"))?;

        if self.write(&json)? == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "failed to write command").into());
        }

        self.wait_for_response(Some(req.request_id))
    }

    pub fn wait_for_event(&mut self, ev: Event) -> Result<()> {
        self.events.clear(); // remove any buffered events
        while !self.events.remove(&ev) {
            let _ = self.wait_for_response::<()>(None)?;
        }
        Ok(())
    }

    fn wait_for_response<T>(&mut self, id: Option<u8>) -> Result<Response<T>>
    where
        for<'de> T: serde::de::Deserialize<'de>,
    {
        if let Some(val) = id.and_then(|id| self.buf.remove(&id)) {
            return Ok(serde_json::from_value(val).unwrap());
        }

        let mut buf = String::new();
        loop {
            self.reader.read_line(&mut buf)?;
            let val = match serde_json::from_str::<Value>(&buf) {
                Ok(val) => val,
                Err(..) => continue,
            };

            if let Some(req) = val
                .get("request_id")
                .and_then(|req| req.as_u64())
                .map(|d| d as u8)
            {
                match id {
                    Some(id) if id == req => {
                        return Ok(serde_json::from_value(val).unwrap());
                    }
                    _ => {}
                };
                self.buf.insert(req, val);
            } else if let Some(ev) = Event::try_from_value(&val) {
                trace!("event: {:?}", ev);
                self.events.insert(ev);
                if id.is_none() {
                    return Ok(Response {
                        data: None,
                        error: "".into(),
                        request_id: 0,
                    });
                }
            }

            buf.clear();
        }
    }

    fn write(&mut self, data: &str) -> Result<usize> {
        let size = self.writer.write(data.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(size)
    }
}

// https://mpv.io/manual/stable/#list-of-events

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum Event {
    StartFile,
    EndFile,
    EndFileReason(Reason), // is this needed?
    FileLoaded,
    Idle,
    Shutdown,
    TracksChanged,
    TrackSwitched,
    Pause,
    Unpause,
    MetadataUpdate,
}

impl Event {
    pub fn try_from_value(val: &Value) -> Option<Self> {
        let name = val.get("event")?;
        let ev = match name.as_str()? {
            "start-file" => Event::StartFile,
            "end-file" => {
                let reason = if let Some(reason) = val.get("reason").and_then(|s| s.as_str()) {
                    match reason {
                        "eof" => Reason::Eof,
                        "stop" => Reason::Stop,
                        "quit" => Reason::Quit,
                        "error" => Reason::Error,
                        "redirect" => Reason::Redirect,
                        _ => return Some(Event::EndFile), // this is bad
                    }
                } else {
                    Reason::Unknown
                };
                Event::EndFileReason(reason)
            }
            "file-loaded" => Event::FileLoaded,
            "idle" => Event::Idle,
            "shutdown" => Event::Shutdown,
            "tracks-changed " => Event::TracksChanged,
            "track-switched" => Event::TrackSwitched,
            "pause" => Event::Pause,
            "unpause" => Event::Unpause,
            "metadata-update" => Event::MetadataUpdate,
            _ => return None,
        };

        Some(ev)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum Reason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Unknown,
}

#[derive(PartialEq)]
#[allow(dead_code)]
pub enum Command {
    LoadFile(String),
    Quit(i64),
    Stop,
    SetProperty(String, Value),
    GetProperty(String),
}

#[allow(dead_code)]
impl Command {
    pub fn get(prop: impl ToString) -> Self {
        Command::GetProperty(prop.to_string())
    }

    pub fn set(prop: impl ToString, value: impl Into<Value>) -> Self {
        Command::SetProperty(prop.to_string(), value.into())
    }

    fn command_list(self) -> Vec<Value> {
        match self {
            Command::LoadFile(file) => vec!["loadfile".into(), file.into()],
            Command::Quit(code) => vec!["quit".into(), code.into()],
            Command::Stop => vec!["stop".into()],
            Command::SetProperty(prop, val) => vec!["set_property".into(), prop.into(), val],
            Command::GetProperty(prop) => vec!["get_property".into(), prop.into()],
        }
    }
}

#[derive(Serialize)]
pub struct Request {
    command: Vec<Value>,
    request_id: u8,
}

impl Request {
    pub fn new(cmd: Command) -> Self {
        Self {
            command: cmd.command_list(),
            request_id: thread_rng().gen(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Response<T> {
    pub data: Option<T>,
    error: String,
    request_id: u8,
}

#[allow(dead_code)]
impl<T> Response<T> {
    pub fn id(&self) -> u8 {
        self.request_id
    }

    pub fn success(&self) -> bool {
        self.error == "success"
    }

    pub fn error(&self) -> &str {
        &self.error
    }
}
