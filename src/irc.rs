use std::collections::HashMap;
use std::str::FromStr;

#[derive(Default, Debug, PartialEq, Clone)]
pub struct Tags(HashMap<String, String>);

impl Tags {
    pub fn parse(input: &str) -> Self {
        let mut map = HashMap::new();
        let input = &input[1..];
        for part in input.split_terminator(';') {
            if let Some(index) = part.find('=') {
                let (k, v) = (&part[..index], &part[index + 1..]);
                map.insert(k.to_owned(), v.to_owned());
            }
        }
        Tags(map)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }

    pub fn badges(&self) -> Option<Vec<Badge>> {
        Some(
            self.0
                .get("badges")?
                .split(',')
                .map(|s| {
                    let mut t = s.split('/');
                    (t.next(), t.next()) // badge, version
                })
                .filter_map(|(s, _)| s.and_then(|s| Badge::from_str(s).ok()))
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Badge {
    Admin,
    Broadcaster,
    GlobalMod,
    Moderator,
    Subscriber,
    Staff,
    Turbo,
}

impl FromStr for Badge {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let res = match s.to_ascii_lowercase().as_str() {
            "admin" => Badge::Admin,
            "broadcaster" => Badge::Broadcaster,
            "global_mod" => Badge::GlobalMod,
            "moderator" => Badge::Moderator,
            "subscriber" => Badge::Subscriber,
            "staff" => Badge::Staff,
            "turbo" => Badge::Turbo,
            _ => return Err(()),
        };
        Ok(res)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum IrcCommand {
    Ping {
        data: String,
    },
    Privmsg {
        target: String,
        sender: String,
        data: String,
    },
    Unknown {
        cmd: String,
        args: Vec<String>,
        data: String,
    },
}

#[derive(Debug, PartialEq, Clone)]
pub struct IrcMessage {
    pub tags: Tags,
    pub command: IrcCommand,
}

impl IrcMessage {
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return None;
        }

        let (input, tags) = if input.starts_with('@') {
            let pos = input.find(' ').unwrap();
            let sub = &input[..pos];
            let tags = Tags::parse(&sub);
            (&input[pos + 1..], tags)
        } else {
            (input, Tags::default())
        };

        fn parse_prefix(input: &str) -> Option<&str> {
            if input.starts_with(':') {
                let s = &input[1..input.find(' ')?];
                Some(match s.find('!') {
                    Some(pos) => &s[..pos],
                    None => s,
                })
            } else {
                None
            }
        }

        let prefix = parse_prefix(&input);
        let mut args = input
            .split_whitespace()
            .skip(if prefix.is_some() { 1 } else { 0 })
            .take_while(|s| !s.starts_with(':'))
            .collect::<Vec<_>>();

        fn get_data(s: &str) -> &str {
            if let Some(pos) = &s[1..].find(':') {
                &s[*pos + 2..]
            } else {
                ""
            }
        }

        let command = match args.remove(0) {
            "PRIVMSG" => IrcCommand::Privmsg {
                target: args.remove(0).into(),
                sender: prefix.unwrap().into(),
                data: get_data(&input).into(),
            },
            "PING" => IrcCommand::Ping {
                data: get_data(&input).into(),
            },
            cmd => IrcCommand::Unknown {
                cmd: cmd.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
                data: get_data(&input).into(),
            },
        };

        Some(IrcMessage { tags, command })
    }
}
