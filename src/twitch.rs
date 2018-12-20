use std::env;
use std::io::prelude::*;
use std::io::{self, BufRead, BufReader, BufWriter};
use std::net::TcpStream;

use std::sync::mpsc;
use std::thread;

use crate::irc::*;
use log::*;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    TwitchPass,
    ParseMessage,
    CannotRead,
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::IoError(err)
    }
}

#[derive(Debug, Copy, Clone)]
pub enum Target<'a> {
    Channel(&'a str),
}

#[derive(Debug, Copy, Clone)]
pub struct Command<'a> {
    pub kind: CommandKind<'a>,
    pub target: Target<'a>,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum CommandKind<'a> {
    Request { id: &'a str, req: &'a str },
    Play { pos: &'a str },
    Info,
    List,
    Skip,
    Random,
}

impl<'a> Command<'a> {
    pub fn parse(msg: &'a IrcMessage) -> Option<Self> {
        use self::CommandKind::*;

        if let (IrcCommand::Privmsg { target, data, .. }, Some(ref badges), Some(id)) =
            (&msg.command, msg.tags.badges(), msg.tags.get("user-id"))
        {
            let check =
                || badges.contains(&Badge::Broadcaster) || badges.contains(&Badge::Moderator);

            let mut parts = data.split_whitespace();
            let kind = match parts.next()? {
                "!songinfo" | "!song" | "!current" => Info,
                "!songlist" | "!list" => List,
                "!songrequest" | "!sr" => Request {
                    id,
                    req: parts.next()?,
                },

                "!play" if check() => Play { pos: parts.next()? },
                "!skip" if check() => Skip,
                "!random" if check() => Random,
                _ => return None,
            };

            let target = Target::Channel(target);

            let cmd = Command { kind, target };
            debug!("got a command: {:?}", cmd);
            Some(cmd)
        } else {
            None
        }
    }
}

pub struct Client {
    writer: BufWriter<TcpStream>,
    buf: mpsc::Receiver<String>,
    quit: mpsc::Sender<()>,
    msg: Option<String>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Client {
    pub fn connect(channel: &str, name: &str) -> Result<Self> {
        let pass = env::var("SHAKEN_TWITCH_PASSWORD").map_err(|_| Error::TwitchPass)?;

        info!("connected");
        let conn = TcpStream::connect("irc.chat.twitch.tv:6667")?;
        let writer = BufWriter::new(conn.try_clone().unwrap());
        let (quit, buf) = Self::run(conn);

        let mut this = Self {
            writer,
            quit,
            buf,
            msg: None,
        };

        this.write("CAP REQ :twitch.tv/tags")?;
        this.write("CAP REQ :twitch.tv/membership")?;
        this.write("CAP REQ :twitch.tv/commands")?;

        this.write(format!("PASS {}", pass))?;
        this.write(format!("NICK {}", name))?;
        this.write(format!("JOIN #{}", channel))?;

        debug!("sent initial handshake");

        Ok(this)
    }

    pub fn reply<'a>(&mut self, target: impl Into<Target<'a>>, data: &str) -> Result<()> {
        let target = target.into();
        match target {
            Target::Channel(ch) => self.write(format!("PRIVMSG {} :{}", ch, data))?,
        };

        Ok(())
    }

    pub fn next_message(&mut self) -> Result<IrcMessage> {
        let msg = self.read()?;
        self.msg.replace(msg);
        self.parse().ok_or_else(|| Error::ParseMessage)
    }

    pub fn write(&mut self, data: impl AsRef<str>) -> Result<()> {
        for data in split(data.as_ref()).iter().map(|s| s.as_bytes()) {
            self.writer.write_all(data)?;
        }
        self.writer.flush().map_err(|e| e.into())
    }

    pub fn stop(&mut self) {
        debug!("sending stop");
        let _ = self.write("QUIT :bye");
        let _ = self.quit.send(());
    }

    fn parse(&mut self) -> Option<IrcMessage> {
        let msg = IrcMessage::parse(&self.msg.as_ref().cloned().unwrap())?;
        if let IrcCommand::Ping { ref data } = msg.command {
            self.write(format!("PONG :{}", &data)).ok()?;
        };
        Some(msg)
    }

    fn read(&mut self) -> Result<String> {
        self.buf.recv().map_err(|_| Error::CannotRead)
    }

    fn run(stream: TcpStream) -> (mpsc::Sender<()>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let (qtx, qrx) = mpsc::channel();

        thread::spawn(move || {
            debug!("starting read loop");
            let mut lines = BufReader::new(stream).lines();
            while let Some(Ok(line)) = lines.next() {
                match qrx.try_recv() {
                    Err(mpsc::TryRecvError::Disconnected) | Ok(..) => {
                        debug!("got a quit signal, ending reading");
                        break;
                    }
                    _ => {}
                }
                if tx.send(line).is_err() {
                    debug!("cannot send, ending read");
                    break;
                }
                match qrx.try_recv() {
                    Err(mpsc::TryRecvError::Disconnected) | Ok(..) => {
                        debug!("got a quit signal, ending reading");
                        break;
                    }
                    _ => {}
                }
            }
            debug!("end of read loop")
        });

        (qtx, rx)
    }
}

fn split(data: &str) -> Vec<String> {
    use std::str;
    if data.len() > 510 && data.contains(':') {
        let mut split = data.splitn(2, ':').map(str::trim);
        let (head, tail) = (split.next().unwrap(), split.next().unwrap());
        return tail
            .as_bytes()
            .chunks(510 - head.len())
            .map(str::from_utf8) // XXX this drops bytes on split boundaries
            .filter_map(|s| s.ok())
            .map(|s| format!("{} :{}\r\n", head, s))
            .collect();
    }
    vec![format!("{}\r\n", data)]
}
