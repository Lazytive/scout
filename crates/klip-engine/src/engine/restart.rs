//! Settings a recorder reads once, at its start: which changes wait for a restart, and
//! `recorder.restart` to apply them now. The UI asks the user "restart now or later"
//! (restarting clears the replay buffer); a full-session recording goes on in a new file.

use klip_control::CommandError;
use klip_settings::{Effective, Settings};
use serde_json::{Value, json};

use super::{Inner, lock};
use crate::events::Event;

/// What a running recorder was started with (the parts of the settings it uses).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct StartedWith {
    eff: Effective,
    folder: Option<String>,
    recordings_folder: Option<String>,
    per_game_folders: bool,
}

impl StartedWith {
    /// The settings a recording of `game` (an executable name) would start with now.
    pub(super) fn new(settings: &Settings, game: Option<&str>) -> StartedWith {
        let mut eff = settings.effective(game);
        // Auto-clip isn't the recorder's (and was dropped from the product).
        eff.auto_clip = false;
        StartedWith {
            eff,
            folder: settings.clips.folder.clone(),
            recordings_folder: settings.clips.recordings_folder.clone(),
            per_game_folders: settings.clips.per_game_folders,
        }
    }

    /// What differs in `now`, named as the Settings page names it.
    pub(super) fn changed(&self, now: &StartedWith) -> Vec<&'static str> {
        let (a, b) = (&self.eff, &now.eff);
        let mut out = Vec::new();
        if a.quality != b.quality {
            out.push("Quality");
        }
        if a.encoder != b.encoder {
            out.push("Encoder");
        }
        if a.buffer_secs != b.buffer_secs {
            out.push("Replay buffer length");
        }
        if a.show_cursor != b.show_cursor {
            out.push("Mouse cursor");
        }
        if a.audio != b.audio {
            out.push("Audio");
        }
        if self.folder != now.folder
            || self.recordings_folder != now.recordings_folder
            || self.per_game_folders != now.per_game_folders
        {
            out.push("Clip folders");
        }
        out
    }
}

impl Inner {
    /// The followed game's executable, if any.
    fn followed_exe(&self) -> Option<String> {
        lock(&self.state).game.as_ref().map(|g| g.exe.clone())
    }

    /// Changed settings the running recorder doesn't use yet (empty: none, or no
    /// recorder).
    pub(super) fn restart_pending(&self) -> Vec<&'static str> {
        let settings = self.settings();
        let exe = self.followed_exe();
        let now = StartedWith::new(&settings, exe.as_deref());
        lock(&self.state)
            .current
            .as_ref()
            .map(|c| c.started_with.changed(&now))
            .unwrap_or_default()
    }

    /// After a settings change from `old` to `new`: tell the UI when a running recorder
    /// is affected (`restart_pending`, also with no reasons when a change was undone).
    pub(super) fn settings_need_restart(&self, old: &Settings, new: &Settings) {
        let exe = self.followed_exe();
        if StartedWith::new(old, exe.as_deref()) == StartedWith::new(new, exe.as_deref()) {
            return;
        }
        if lock(&self.state).current.is_none() {
            return;
        }
        let reasons = self
            .restart_pending()
            .into_iter()
            .map(String::from)
            .collect();
        self.emit(&Event::RestartPending { reasons });
    }

    /// `recorder.restart`: stop the recorder (a recording is finalised) and start it
    /// again at once with the current settings; a recording that ran goes on in a new
    /// file. The replay buffer starts empty.
    pub(super) fn restart_recorder(&self) -> Result<Value, CommandError> {
        let Some(rec) = lock(&self.state)
            .current
            .as_ref()
            .map(|c| c.recorder.clone())
        else {
            return Ok(json!({ "restarting": false }));
        };
        let recording = rec.status().is_ok_and(|(_, session)| session);
        drop(rec);
        self.stop_recorder("settings changed");
        {
            let mut st = lock(&self.state);
            st.retry_at = None;
            st.resume_session = recording;
        }
        self.wake();
        Ok(json!({ "restarting": true, "resumes_recording": recording }))
    }
}

#[cfg(test)]
mod tests {
    use klip_settings::{DesktopAudio, Preset};

    use super::*;

    #[test]
    fn only_what_the_recorder_reads_needs_a_restart() {
        let s = Settings::default();
        let base = StartedWith::new(&s, None);
        assert!(base.changed(&StartedWith::new(&s, None)).is_empty());

        let mut t = s.clone();
        t.hotkeys.clear();
        t.storage.cap_gb = t.storage.cap_gb.saturating_add(1);
        t.app.start_minimized = !t.app.start_minimized;
        assert!(
            base.changed(&StartedWith::new(&t, None)).is_empty(),
            "applied live"
        );

        let mut t = s.clone();
        t.recording.quality.preset = if s.recording.quality.preset == Preset::High {
            Preset::Low
        } else {
            Preset::High
        };
        t.recording.buffer_secs = s.recording.buffer_secs.saturating_add(10);
        t.audio.desktop = if s.audio.desktop == DesktopAudio::Off {
            DesktopAudio::System
        } else {
            DesktopAudio::Off
        };
        t.clips.per_game_folders = !s.clips.per_game_folders;
        assert_eq!(
            base.changed(&StartedWith::new(&t, None)),
            ["Quality", "Replay buffer length", "Audio", "Clip folders"]
        );
    }

    #[test]
    fn a_game_profile_counts_for_its_game_only() {
        let s = Settings::default();
        let mut t = s.clone();
        let v = serde_json::json!({"games": {"profiles": {"game.exe": {"buffer_secs": 45}}}});
        t = t.apply_patch(&v).unwrap_or(t);
        let changed =
            StartedWith::new(&s, Some("Game.exe")).changed(&StartedWith::new(&t, Some("Game.exe")));
        assert_eq!(changed, ["Replay buffer length"]);
        let other = StartedWith::new(&s, Some("Other.exe"))
            .changed(&StartedWith::new(&t, Some("Other.exe")));
        assert!(other.is_empty());
    }
}
