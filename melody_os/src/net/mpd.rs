use alloc::string::String;
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
}
