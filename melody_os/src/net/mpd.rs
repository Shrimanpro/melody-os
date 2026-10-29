//! Music Player Daemon protocol (the subset clients like mpc, ncmpcpp, M.A.L.P. and
//! Cantata need). The "queue" is the whole music library in path order; song ids are the
//! library indices.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use crate::audio::player::{PlayState, Player};

pub const GREETING: &[u8] = b"OK MPD 0.23.5\n";

const ACK_ARG: u32 = 2;
const ACK_UNKNOWN: u32 = 5;
const ACK_NO_EXIST: u32 = 50;

/// Per-connection protocol state.
pub struct Client {
    line: Vec<u8>,
    pub out: Vec<u8>,
    list: Option<CommandList>,
    idle: Option<u32>, // player version seen when `idle` started
    pub close: bool,
}

struct CommandList {
    ok_each: bool,
    cmds: Vec<String>,
}

impl Client {
    pub fn new() -> Self {
        let mut c = Client { line: Vec::new(), out: Vec::new(), list: None, idle: None, close: false };
        c.out.extend_from_slice(GREETING);
        c
    }

    /// Feed bytes received from the socket; complete lines are executed.
    pub fn feed(&mut self, data: &[u8], player: &mut Player) {
        for &b in data {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.line).trim_end_matches('\r').into();
                self.line.clear();
                self.handle_line(line, player);
            } else if self.line.len() < 4096 {
                self.line.push(b);
            }
        }
    }

    /// Called every poll: completes a pending `idle` once something changed.
    pub fn check_idle(&mut self, player: &Player) {
        if let Some(seen) = self.idle {
            if player.version != seen {
                self.idle = None;
                self.out.extend_from_slice(b"changed: player\nchanged: mixer\nchanged: output\nOK\n");
            }
        }
    }

    fn handle_line(&mut self, line: String, player: &mut Player) {
        if self.idle.is_some() {
            // While idle only `noidle` is allowed.
            if line.trim() == "noidle" {
                self.idle = None;
                self.out.extend_from_slice(b"OK\n");
            } else {
                self.close = true;
            }
            return;
        }
        if let Some(list) = self.list.as_mut() {
            if line.trim() == "command_list_end" {
                let list = self.list.take().unwrap();
                for (i, cmd) in list.cmds.iter().enumerate() {
                    let mut resp = String::new();
                    if let Err((code, msg)) = execute(cmd, player, &mut resp, self) {
                        let name = cmd.split_whitespace().next().unwrap_or("");
                        let _ = write!(self.out_str(), "ACK [{}@{}] {{{}}} {}\n", code, i, name, msg);
                        return;
                    }
                    self.out.extend_from_slice(resp.as_bytes());
                    if list.ok_each {
                        self.out.extend_from_slice(b"list_OK\n");
                    }
                }
                self.out.extend_from_slice(b"OK\n");
            } else {
                list.cmds.push(line);
            }
            return;
        }

        match line.trim() {
            "command_list_begin" => self.list = Some(CommandList { ok_each: false, cmds: Vec::new() }),
            "command_list_ok_begin" => self.list = Some(CommandList { ok_each: true, cmds: Vec::new() }),
            "" => {}
            _ => {
                let mut resp = String::new();
                match execute(&line, player, &mut resp, self) {
                    Ok(()) => {
                        if self.idle.is_none() && !self.close {
                            self.out.extend_from_slice(resp.as_bytes());
                            self.out.extend_from_slice(b"OK\n");
                        }
                    }
                    Err((code, msg)) => {
                        let name = line.split_whitespace().next().unwrap_or("");
                        let _ = write!(self.out_str(), "ACK [{}@0] {{{}}} {}\n", code, name, msg);
                    }
                }
            }
        }
    }

    fn out_str(&mut self) -> OutWriter<'_> {
        OutWriter(&mut self.out)
    }
}

struct OutWriter<'a>(&'a mut Vec<u8>);

impl Write for OutWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Split a command line into words, honouring double quotes and backslash escapes.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut have = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                have = true;
            }
            '\\' if in_quotes => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if have {
                    out.push(core::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

type CmdResult = Result<(), (u32, &'static str)>;

fn arg_num(args: &[String], i: usize) -> Result<i64, (u32, &'static str)> {
    args.get(i).and_then(|s| s.parse::<i64>().ok()).ok_or((ACK_ARG, "need a number"))
}

/// Format milliseconds as MPD's "seconds.millis".
fn secs(ms: u64) -> String {
    alloc::format!("{}.{:03}", ms / 1000, ms % 1000)
}

fn song_info(out: &mut String, player: &Player, idx: usize) {
    let t = &player.tracks()[idx];
    let _ = write!(out, "file: {}\nTitle: {}\nPos: {}\nId: {}\n", t.path, t.title(), idx, idx);
    // Folder layout Artist/Album/Song is common: expose it as tags when present.
    let parts: Vec<&str> = t.path.split('/').collect();
    if parts.len() >= 3 {
        let _ = write!(out, "Artist: {}\nAlbum: {}\n", parts[parts.len() - 3], parts[parts.len() - 2]);
    } else if parts.len() == 2 {
        let _ = write!(out, "Album: {}\n", parts[0]);
    }
}

fn execute(line: &str, player: &mut Player, out: &mut String, client: &mut Client) -> CmdResult {
    let args = tokenize(line);
    let Some(cmd) = args.first() else { return Ok(()) };
    let count = player.track_count();

    match cmd.as_str() {
        "ping" | "clearerror" | "binarylimit" | "random" | "single" | "consume" | "crossfade"
        | "replay_gain_mode" | "subscribe" | "unsubscribe" | "password" | "clear" => {}
        "close" => client.close = true,
        "idle" => client.idle = Some(player.version),
        "noidle" => {}
        "status" => {
            let s = player.status();
            let _ = write!(
                out,
                "volume: {}\nrepeat: {}\nrandom: 0\nsingle: 0\nconsume: 0\nplaylist: 1\nplaylistlength: {}\nmixrampdb: 0\nstate: {}\n",
                if s.muted { 0 } else { s.volume },
                s.repeat as u8,
                count,
                s.state.mpd_name()
            );
            if let Some(i) = s.track {
                let _ = write!(out, "song: {}\nsongid: {}\n", i, i);
                if i + 1 < count {
                    let _ = write!(out, "nextsong: {}\nnextsongid: {}\n", i + 1, i + 1);
                }
            }
            if s.state != PlayState::Stopped {
                let _ = write!(out, "time: {}:{}\nelapsed: {}\n", s.elapsed_ms / 1000, s.duration_ms / 1000, secs(s.elapsed_ms));
                if s.duration_ms > 0 {
                    let _ = write!(out, "duration: {}\n", secs(s.duration_ms));
                }
                if let Some(info) = s.info {
                    let _ = write!(out, "bitrate: {}\naudio: {}:{}:2\n", info.kbps, info.sample_rate, info.bits);
                }
            }
        }
        "currentsong" => {
            if let Some(i) = player.status().track {
                song_info(out, player, i);
            }
        }
        "playlistinfo" | "playlistid" => {
            match args.get(1).and_then(|s| s.split(':').next()?.parse::<usize>().ok()) {
                Some(i) if i < count => song_info(out, player, i),
                Some(_) => return Err((ACK_ARG, "Bad song index")),
                None => {
                    for i in 0..count {
                        song_info(out, player, i);
                    }
                }
            }
        }
        "play" | "playid" => {
            let idx = if args.len() > 1 { arg_num(&args, 1)? } else { -1 };
            if idx < 0 {
                if player.state() == PlayState::Paused {
                    player.resume();
                } else if player.state() == PlayState::Stopped {
                    player.toggle_pause();
                }
            } else if (idx as usize) < count {
                player.play(idx as usize);
            } else {
                return Err((ACK_ARG, "Bad song index"));
            }
        }
        "pause" => match args.get(1).map(|s| s.as_str()) {
            Some("1") => player.pause(),
            Some("0") => player.resume(),
            _ => player.toggle_pause(),
        },
        "stop" => player.stop(),
        "next" => player.next(),
        "previous" => player.prev(),
        "setvol" => {
            let v = arg_num(&args, 1)?;
            if !(0..=100).contains(&v) {
                return Err((ACK_ARG, "Invalid volume value"));
            }
            player.set_volume(v as u8);
        }
        "volume" => player.change_volume(arg_num(&args, 1)? as i32),
        "getvol" => {
            let _ = write!(out, "volume: {}\n", player.volume());
        }
        "repeat" => player.repeat = arg_num(&args, 1)? != 0,
        "outputs" => {
            let active = player.active_output();
            for (i, o) in player.outputs().iter().enumerate() {
                let _ = write!(
                    out,
                    "outputid: {}\noutputname: {}\nplugin: hda\noutputenabled: {}\n",
                    i,
                    o.kind.name(),
                    (Some(*o) == active) as u8
                );
            }
        }
        "enableoutput" | "toggleoutput" => {
            let i = arg_num(&args, 1)? as usize;
            if i >= player.outputs().len() {
                return Err((ACK_NO_EXIST, "No such audio output"));
            }
            player.select_output(i);
        }
        // Exactly one output is active at a time; disabling it is a no-op.
        "disableoutput" => {}
        "add" | "addid" => {
            let uri = args.get(1).ok_or((ACK_ARG, "need a URI"))?;
            let idx = player.tracks().iter().position(|t| &t.path == uri).ok_or((ACK_NO_EXIST, "No such song"))?;
            if cmd == "addid" {
                let _ = write!(out, "Id: {}\n", idx);
            }
        }
        "lsinfo" | "listall" | "listallinfo" => {
            let prefix = args.get(1).map(|s| s.trim_matches('/')).unwrap_or("");
            let recursive = cmd != "lsinfo";
            list_dir(out, player, prefix, recursive);
        }
        "list" => {
            // Only "list file" style queries are meaningful without tags.
            if args.get(1).map(|s| s.eq_ignore_ascii_case("file")).unwrap_or(false) {
                for t in player.tracks() {
                    let _ = write!(out, "file: {}\n", t.path);
                }
            }
        }
        "stats" => {
            let _ = write!(out, "artists: 0\nalbums: 0\nsongs: {}\nuptime: {}\nplaytime: 0\ndb_playtime: 0\n", count, crate::interrupts::now_ms() / 1000);
        }
        "commands" => {
            for c in [
                "add", "addid", "clear", "clearerror", "close", "commands", "currentsong", "disableoutput", "enableoutput",
                "getvol", "idle", "list", "listall", "listallinfo", "lsinfo", "next", "noidle", "notcommands", "outputs",
                "pause", "ping", "play", "playid", "playlistid", "playlistinfo", "previous", "repeat", "setvol", "stats",
                "status", "stop", "tagtypes", "toggleoutput", "urlhandlers", "volume",
            ] {
                let _ = write!(out, "command: {}\n", c);
            }
        }
        "notcommands" | "urlhandlers" | "decoders" | "listplaylists" | "listmounts" | "listneighbors" => {}
        "tagtypes" => {
            if args.len() == 1 {
                out.push_str("tagtype: Artist\ntagtype: Album\ntagtype: Title\n");
            }
        }
        "replay_gain_status" => out.push_str("replay_gain_mode: off\n"),
        _ => return Err((ACK_UNKNOWN, "unknown command")),
    }
    Ok(())
}

fn list_dir(out: &mut String, player: &Player, prefix: &str, recursive: bool) {
    let mut dirs: Vec<&str> = Vec::new();
    for (i, t) in player.tracks().iter().enumerate() {
        let rest = if prefix.is_empty() {
            t.path.as_str()
        } else if let Some(r) = t.path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
            r
        } else {
            continue;
        };
        match rest.find('/') {
            Some(slash) if !recursive => {
                let dir = &t.path[..t.path.len() - rest.len() + slash];
                if dirs.last() != Some(&dir) {
                    dirs.push(dir);
                    let _ = write!(out, "directory: {}\n", dir);
                }
            }
            _ => {
                if recursive {
                    let _ = write!(out, "file: {}\n", t.path);
                } else {
                    song_info(out, player, i);
                }
            }
        }
    }
}

#[test_case]
fn test_tokenize_quotes() {
    let t = tokenize(r#"add "Artist/My \"Best\" Song.flac""#);
    assert_eq!(t.len(), 2);
    assert_eq!(t[1], "Artist/My \"Best\" Song.flac");
    assert_eq!(tokenize("  setvol   50 ").len(), 2);
}
