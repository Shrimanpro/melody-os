use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpdPlaybackState {
    Play,
    Pause,
    Stop,
}

pub struct MpdServer {
    pub state: MpdPlaybackState,
    pub volume: u8,
    pub playlist: Vec<String>,
    pub current_song_idx: Option<usize>,
}

impl MpdServer {
    pub fn new() -> Self {
        MpdServer {
            state: MpdPlaybackState::Stop,
            volume: 100,
            playlist: Vec::new(),
            current_song_idx: None,
        }
    }

    pub fn set_playlist(&mut self, files: Vec<String>) {
        if !files.is_empty() && self.current_song_idx.is_none() {
            self.current_song_idx = Some(0);
        }
        self.playlist = files;
    }

    /// Welcome message sent when client establishes connection on port 6600
    pub fn welcome_banner(&self) -> &'static str {
        "OK MPD 0.23.5\n"
    }

    /// Process a line received from the MPD client
    pub fn handle_command(&mut self, line: &str) -> String {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        let cmd = match parts.next() {
            Some(c) => c,
            None => return "OK\n".to_string(),
        };

        match cmd {
            "ping" => "OK\n".to_string(),

            "status" => {
                let state_str = match self.state {
                    MpdPlaybackState::Play => "play",
                    MpdPlaybackState::Pause => "pause",
                    MpdPlaybackState::Stop => "stop",
                };
                let mut out = format!(
                    "volume: {}\nrepeat: 0\nrandom: 0\nsingle: 0\nconsume: 0\nplaylist: 1\nplaylistlength: {}\nstate: {}\n",
                    self.volume,
                    self.playlist.len(),
                    state_str
                );
                if let Some(idx) = self.current_song_idx {
                    out.push_str(&format!("song: {}\nsongid: {}\n", idx, idx));
                }
                out.push_str("OK\n");
                out
            }

            "currentsong" => {
                if let Some(idx) = self.current_song_idx {
                    if let Some(track) = self.playlist.get(idx) {
                        return format!(
                            "file: {}\nTitle: {}\nPos: {}\nId: {}\nOK\n",
                            track, track, idx, idx
                        );
                    }
                }
                "OK\n".to_string()
            }

            "play" | "playid" => {
                if let Some(arg) = parts.next() {
                    if let Ok(idx) = arg.parse::<usize>() {
                        if idx < self.playlist.len() {
                            self.current_song_idx = Some(idx);
                        }
                    }
                }
                self.state = MpdPlaybackState::Play;
                crate::println!("MPD: State changed to PLAY");
                "OK\n".to_string()
            }

            "stop" => {
                self.state = MpdPlaybackState::Stop;
                crate::println!("MPD: State changed to STOP");
                "OK\n".to_string()
            }

            "pause" => {
                let arg = parts.next().unwrap_or("1");
                if arg == "0" {
                    self.state = MpdPlaybackState::Play;
                } else {
                    self.state = MpdPlaybackState::Pause;
                }
                "OK\n".to_string()
            }

            "setvol" => {
                if let Some(arg) = parts.next() {
                    if let Ok(vol) = arg.parse::<u8>() {
                        self.volume = vol.min(100);
                    }
                }
                "OK\n".to_string()
            }

            "outputs" => {
                "outputid: 0\noutputname: S/PDIF Optical (Intel HDA)\noutputenabled: 1\noutputid: 1\noutputname: Analog Line Out (Intel HDA)\noutputenabled: 1\nOK\n".to_string()
            }

            "lsinfo" | "listall" => {
                let mut out = String::new();
                for track in &self.playlist {
                    out.push_str(&format!("file: {}\n", track));
                }
                out.push_str("OK\n");
                out
            }

            "commands" => {
                "command: status\ncommand: currentsong\ncommand: play\ncommand: stop\ncommand: pause\ncommand: setvol\ncommand: outputs\ncommand: lsinfo\ncommand: ping\ncommand: close\nOK\n".to_string()
            }

            "notcommands" => "OK\n".to_string(),
            "tagtypes" => "OK\n".to_string(),
            "close" => "OK\n".to_string(),

            _ => {
                format!("ACK [5@0] {{}} unknown command \"{}\"\n", cmd)
            }
        }
    }
}
