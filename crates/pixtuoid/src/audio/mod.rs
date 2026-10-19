//! Ambient office audio — the only owner of any audio-device dependency, over the pure
//! synthesis in `pixtuoid_scene::audio`; every sound is ALL-PROCEDURAL, synthesized on
//! the playback thread, which sits behind a bounded `try_send` channel.

#[cfg(feature = "audio")]
mod music;
#[cfg(feature = "audio")]
pub(crate) mod sink;

use std::sync::mpsc;
#[cfg(feature = "audio")]
use std::time::Instant;

use pixtuoid_scene::audio::AudioFrame;
#[cfg(all(feature = "audio", test))]
use pixtuoid_scene::audio::OneShot;
#[cfg(feature = "audio")]
use pixtuoid_scene::audio::mixer::LoopStem;
#[cfg(feature = "audio")]
use pixtuoid_scene::audio::{AudioEngine, MAX_DT_S};
#[cfg(feature = "audio")]
use sink::AudioSink;

#[cfg(feature = "audio")]
use pixtuoid_scene::audio::bank::Synthesis;

/// The +/- keys' volume increment.
pub(crate) const VOLUME_STEP: f32 = 0.05;
/// How long the transient volume readout stays up after a nudge — also the
/// volume-persist debounce window on both painters.
pub(crate) const VOLUME_FLASH_MS: u128 = 1000;

/// The two gestures both painters drive; the KEY→action map is painter-specific, the state
/// transition ([`apply_audio_action`]) is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioAction {
    ToggleMute,
    /// `true` = volume up.
    Volume(bool),
}

pub(crate) struct AudioUi {
    pub(crate) handle: AudioHandle,
    pub(crate) muted: bool,
    pub(crate) volume: f32,
}

/// What the caller persists after [`apply_audio_action`] — the side effects stay
/// painter-side so the transition itself is pure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Persist {
    /// The mute flag CHANGED — persist now.
    pub(crate) muted: bool,
    /// A volume key was pressed, even one clamped at a rail — flash the readout
    /// and persist DEBOUNCED (the +/- keys autorepeat).
    pub(crate) volume_nudged: bool,
}

/// THE audio mute/volume transition — the single authority both painters run. Mute toggles;
/// volume-up from muted IS the un-mute gesture; the lazy spawn (re)fires whenever sound is
/// wanted but the system is down, so `+`/`m` are never dead keys (boot-muted and
/// failed-spawn both recover). `paused` folds an external hold (the TUI's `[p]ause`) into
/// the effective mute; floating passes `false`.
pub(crate) fn apply_audio_action(
    st: &mut AudioUi,
    action: AudioAction,
    paused: bool,
    respawn: impl FnOnce(&AudioHandle, f32),
) -> Persist {
    let mut persist = Persist {
        muted: false,
        volume_nudged: false,
    };
    match action {
        AudioAction::ToggleMute => {
            st.muted = !st.muted;
            persist.muted = true;
        }
        AudioAction::Volume(up) => {
            let delta = if up { VOLUME_STEP } else { -VOLUME_STEP };
            st.volume = (st.volume + delta).clamp(0.0, 1.0);
            if up && st.muted {
                st.muted = false;
                persist.muted = true;
            }
            persist.volume_nudged = true;
        }
    }
    if !st.muted && !st.handle.is_enabled() {
        respawn(&st.handle, st.volume);
    }
    st.handle.set_muted(paused || st.muted);
    st.handle.set_volume(st.volume);
    persist
}

/// The mute/volume PERSIST protocol both painters share: the pure
/// [`apply_audio_action`] transition plus its side effects — mute saves NOW, a
/// volume nudge arms the `♩ N%` readout and a save debounced behind it (a held
/// `+`/`-` writes once, not per repeat), flushed on shutdown.
pub(crate) struct AudioController {
    ui: AudioUi,
    config_path: std::path::PathBuf,
    volume_dirty: bool,
    /// When the `♩ N%` readout was armed. Doubles as the debounce clock: the
    /// volume save lands once this window elapses.
    flash_at: Option<std::time::Instant>,
}

impl AudioController {
    /// Construct the controller AND own the device thread's whole lifecycle:
    /// boot-spawn here (iff a persisted unmute wants sound), tear down via
    /// [`Self::stop`], with `Drop` as the net for every other exit path. Each
    /// painter builds it AFTER its fallible boot steps, so no device thread can
    /// exist before its Drop-owner.
    pub(crate) fn new(cfg: crate::config::AudioConfig, config_path: std::path::PathBuf) -> Self {
        Self::new_with(cfg, config_path, respawn)
    }

    /// [`Self::new`] with the boot-spawn injected, so a test can build an UNMUTED
    /// controller without opening an output device — `pub(crate)` because `tui`'s
    /// key-action tests need one.
    pub(crate) fn new_with(
        cfg: crate::config::AudioConfig,
        config_path: std::path::PathBuf,
        respawn: impl FnOnce(&AudioHandle, f32),
    ) -> Self {
        let crate::config::AudioConfig { muted, volume } = cfg;
        let handle = AudioHandle::disabled();
        if !muted {
            respawn(&handle, volume);
        }
        Self {
            ui: AudioUi {
                handle,
                muted,
                volume,
            },
            config_path,
            volume_dirty: false,
            flash_at: None,
        }
    }

    /// Run one gesture: the shared transition, then persist — mute NOW, volume debounced.
    pub(crate) fn apply(
        &mut self,
        action: AudioAction,
        paused: bool,
        now: std::time::Instant,
        respawn: impl FnOnce(&AudioHandle, f32),
    ) {
        let persist = apply_audio_action(&mut self.ui, action, paused, respawn);
        if persist.muted
            && let Err(e) = crate::config::save_audio_muted(&self.config_path, self.ui.muted)
        {
            tracing::warn!(error = ?e, "failed to persist audio mute");
        }
        if persist.volume_nudged {
            self.volume_dirty = true;
            self.flash_at = Some(now);
        }
    }

    fn flashing(&self, now: std::time::Instant) -> bool {
        self.flash_at
            .is_some_and(|t| now.duration_since(t).as_millis() < VOLUME_FLASH_MS)
    }

    /// The `♩ N%` volume readout, `Some` iff the flash window is still fresh.
    pub(crate) fn volume_flash(&self, now: std::time::Instant) -> Option<u8> {
        self.flashing(now)
            .then(|| (self.ui.volume * 100.0).round() as u8)
    }

    /// Per frame: flush the debounced volume save once its window has elapsed.
    pub(crate) fn tick(&mut self, now: std::time::Instant) {
        if self.volume_dirty && !self.flashing(now) {
            self.save_volume();
        }
    }

    fn flush_on_exit(&mut self) {
        if self.volume_dirty {
            self.save_volume();
        }
    }

    fn save_volume(&mut self) {
        self.volume_dirty = false;
        if let Err(e) = crate::config::save_audio_volume(&self.config_path, self.ui.volume) {
            tracing::warn!(error = ?e, "failed to persist audio volume");
        }
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.ui.handle.set_muted(paused || self.ui.muted);
    }

    pub(crate) fn handle(&self) -> &AudioHandle {
        &self.ui.handle
    }

    /// The ONE exit verb for BOTH halves of the audio protocol: PERSIST a
    /// pending debounced volume, THEN stop the device thread. Both run
    /// unconditionally, so a Ctrl-C / terminate / error can't lose a nudge that
    /// landed inside the debounce window. Both halves are panic-free, so this
    /// is safe to run during unwind. Idempotent, so a painter can run it ahead
    /// of the drop to order the join against its own teardown.
    pub(crate) fn stop(&mut self) {
        self.flush_on_exit();
        self.ui.handle.shutdown();
    }
}

/// The net under [`AudioController::stop`] for every exit path that never
/// called it (an error, an unwind). `stop` is idempotent, so a caller that ran
/// it first leaves this nothing to do.
impl Drop for AudioController {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    use crate::config::AudioConfig;
    use std::time::{Duration, Instant};

    fn ctl(muted: bool, volume: f32) -> (AudioController, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"normal\"\n").unwrap();
        // A no-op boot-spawn: the fixture must never open a real device.
        let c = AudioController::new_with(AudioConfig { muted, volume }, path, |_, _| {});
        (c, dir)
    }

    #[test]
    fn new_boot_spawns_only_when_unmuted_and_drop_joins_the_device_thread() {
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"normal\"\n").unwrap();

        let muted_spawned = std::cell::Cell::new(false);
        let c = AudioController::new_with(
            AudioConfig {
                muted: true,
                volume: 0.4,
            },
            path.clone(),
            |_, _| muted_spawned.set(true),
        );
        assert!(!muted_spawned.get(), "a muted boot spawns no device thread");
        assert!(!c.handle().is_enabled());
        drop(c);

        let done = std::cell::OnceCell::new();
        let got_vol = std::cell::Cell::new(0.0f32);
        let c = AudioController::new_with(
            AudioConfig {
                muted: false,
                volume: 0.6,
            },
            path,
            |h, v| {
                got_vol.set(v);
                let _ = done.set(install_slow_fake_device(h));
            },
        );
        assert_eq!(
            got_vol.get(),
            0.6,
            "an unmuted boot spawns at the kept volume"
        );
        assert!(c.handle().is_enabled());

        drop(c);
        assert!(
            done.get().unwrap().load(Ordering::SeqCst),
            "dropping the controller must JOIN the boot-spawned device thread so \
             its teardown completes — the RAII teardown-on-quit guarantee"
        );
    }

    /// `stop` persists and joins once; the drop after it, which production runs
    /// at every floating quit, does nothing more and does not panic.
    #[test]
    fn stop_then_drop_persists_and_joins_once() {
        use std::sync::atomic::Ordering;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"normal\"\n").unwrap();
        let done = std::cell::OnceCell::new();
        let mut c = AudioController::new_with(
            AudioConfig {
                muted: false,
                volume: 0.5,
            },
            path.clone(),
            |h, _| {
                let _ = done.set(install_slow_fake_device(h));
            },
        );
        c.apply(AudioAction::Volume(false), false, Instant::now(), |_, _| {});
        c.stop();
        assert!(done.get().unwrap().load(Ordering::SeqCst), "stop joins");
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("volume"),
            "stop persists the pending nudge"
        );
        std::fs::write(&path, "theme = \"normal\"\n").unwrap();
        drop(c);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "theme = \"normal\"\n",
            "the drop after stop writes nothing"
        );
    }

    #[test]
    fn mute_persists_immediately_and_does_not_arm_the_volume_flash() {
        let (mut c, _d) = ctl(false, 0.4);
        let t0 = Instant::now();
        c.apply(AudioAction::ToggleMute, false, t0, |_, _| {});
        assert!(
            std::fs::read_to_string(&c.config_path)
                .unwrap()
                .contains("muted = true"),
            "mute toggled on AND persists NOW (like a theme commit)"
        );
        assert_eq!(c.volume_flash(t0), None, "mute does not flash ♩ N%");
    }

    #[test]
    fn volume_flashes_now_and_debounces_the_save_until_the_window_elapses() {
        let (mut c, _d) = ctl(false, 0.50);
        let t0 = Instant::now();
        let saved = |c: &AudioController| std::fs::read_to_string(&c.config_path).unwrap();
        c.apply(AudioAction::Volume(true), false, t0, |_, _| {});
        assert_eq!(c.volume_flash(t0), Some(55), "readout armed immediately");
        assert!(
            !saved(&c).contains("volume"),
            "volume NOT persisted mid-flash (debounced, not per-repeat)"
        );
        c.tick(t0 + Duration::from_millis(500));
        assert!(
            !saved(&c).contains("volume"),
            "still within the window → no flush"
        );
        let after = t0 + Duration::from_millis(VOLUME_FLASH_MS as u64 + 50);
        c.tick(after);
        assert!(
            saved(&c).contains("volume"),
            "window elapsed → debounced save flushes"
        );
        assert_eq!(c.volume_flash(after), None, "readout expired");
    }

    #[test]
    fn flush_on_exit_writes_a_pending_nudge() {
        let (mut c, _d) = ctl(false, 0.50);
        c.apply(AudioAction::Volume(false), false, Instant::now(), |_, _| {});
        c.flush_on_exit();
        assert!(
            std::fs::read_to_string(&c.config_path)
                .unwrap()
                .contains("volume"),
            "a nudge-then-quit persists on exit"
        );
    }

    #[test]
    fn drop_persists_a_pending_nudge_even_without_the_q_path() {
        let (mut c, _dir) = ctl(false, 0.50);
        let path = c.config_path.clone();
        c.apply(AudioAction::Volume(false), false, Instant::now(), |_, _| {});
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("volume"),
            "not yet persisted — still inside the debounce window"
        );
        drop(c); // the ONLY exit signal: no `q`, no explicit flush_on_exit()
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("volume"),
            "AudioController::drop must persist a pending nudge (the #752 Ctrl-C fix)"
        );
    }

    #[test]
    fn a_clean_drop_does_not_rewrite_the_config() {
        let (c, _dir) = ctl(false, 0.50);
        let path = c.config_path.clone();
        let before = std::fs::read_to_string(&path).unwrap();
        drop(c);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "an un-dirtied drop must not touch the user's config"
        );
    }
}

#[cfg(test)]
mod controls_tests {
    use super::*;

    #[test]
    fn shutdown_joins_the_device_thread_so_its_teardown_runs_before_return() {
        use std::sync::atomic::Ordering;

        let handle = AudioHandle::disabled();
        let done = install_slow_fake_device(&handle);

        let t0 = std::time::Instant::now();
        handle.shutdown();

        assert!(
            done.load(Ordering::SeqCst),
            "shutdown() must JOIN the device thread so its teardown (the RodioSink \
             Drop that closes the OS device) completes before it returns — without \
             the join it returns mid-teardown and the OS output is stranded"
        );
        assert!(
            t0.elapsed() >= std::time::Duration::from_millis(FAKE_TEARDOWN_MS),
            "shutdown() returned before the thread's teardown could finish — it did \
             not actually wait"
        );
        assert!(
            !handle.is_enabled(),
            "shutdown() drops the sole sender — the handle is inert afterwards"
        );
    }

    #[test]
    fn unmute_lazy_spawns_and_mute_back_does_not() {
        let mut st = AudioUi {
            handle: AudioHandle::disabled(),
            muted: true,
            volume: 0.4,
        };
        let mut spawned_at = None;
        let p = apply_audio_action(&mut st, AudioAction::ToggleMute, false, |h, v| {
            spawned_at = Some(v);
            h.install_test_channel();
        });
        assert!(!st.muted);
        assert_eq!(
            spawned_at,
            Some(0.4),
            "first unmute spawns at the kept volume"
        );
        assert!(st.handle.is_enabled() && !st.handle.is_muted());
        assert_eq!(
            p,
            Persist {
                muted: true,
                volume_nudged: false
            }
        );
        let p = apply_audio_action(&mut st, AudioAction::ToggleMute, false, |_, _| {
            panic!("mute must never spawn")
        });
        assert!(st.muted && st.handle.is_muted());
        assert_eq!(
            p,
            Persist {
                muted: true,
                volume_nudged: false
            }
        );
    }

    #[test]
    fn volume_up_from_muted_unmutes_and_respawns_a_dead_system() {
        let mut st = AudioUi {
            handle: AudioHandle::disabled(),
            muted: true,
            volume: 0.5,
        };
        let mut spawns = 0;
        let p = apply_audio_action(&mut st, AudioAction::Volume(true), false, |h, _| {
            spawns += 1;
            h.install_test_channel();
        });
        assert!(!st.muted, "volume-up IS the un-mute gesture");
        assert_eq!(spawns, 1);
        assert_eq!(
            p,
            Persist {
                muted: true,
                volume_nudged: true
            }
        );
        assert!((st.volume - (0.5 + VOLUME_STEP)).abs() < 1e-6);
        assert!((st.handle.volume() - st.volume).abs() < 1e-6);
        let p = apply_audio_action(&mut st, AudioAction::Volume(false), false, |_, _| {
            panic!("live system must not respawn")
        });
        assert_eq!(
            p,
            Persist {
                muted: false,
                volume_nudged: true
            }
        );
        assert!((st.volume - 0.5).abs() < 1e-6);
    }

    #[test]
    fn volume_clamps_at_both_rails() {
        let (live, _rx) = AudioHandle::test_pair();
        let mut st = AudioUi {
            handle: live,
            muted: false,
            volume: 1.0,
        };
        apply_audio_action(
            &mut st,
            AudioAction::Volume(true),
            false,
            |_, _| unreachable!(),
        );
        assert_eq!(st.volume, 1.0, "top rail");
        st.volume = 0.0;
        apply_audio_action(
            &mut st,
            AudioAction::Volume(false),
            false,
            |_, _| unreachable!(),
        );
        assert_eq!(st.volume, 0.0, "bottom rail");
    }

    #[test]
    fn paused_forces_effective_mute_without_touching_the_flag() {
        let (live, _rx) = AudioHandle::test_pair();
        let mut st = AudioUi {
            handle: live,
            muted: false,
            volume: 0.5,
        };
        apply_audio_action(
            &mut st,
            AudioAction::Volume(true),
            true,
            |_, _| unreachable!(),
        );
        assert!(!st.muted, "the user's flag stays unmuted");
        assert!(st.handle.is_muted(), "but paused silences the handle");
    }

    #[test]
    fn frame_on_a_disabled_handle_does_not_panic_and_an_enabled_one_delivers() {
        let handle = AudioHandle::disabled();
        handle.frame(AudioFrame::default());
        let rx = handle.install_test_channel();
        handle.frame(AudioFrame::default());
        assert_eq!(drain_frames(&rx).len(), 1, "an enabled handle delivers");
    }

    #[test]
    fn a_consumer_clone_survives_a_lazy_respawn_in_place() {
        let handle = AudioHandle::disabled();
        let cached = handle.clone(); // what a renderer caches once, at init
        assert!(!cached.is_enabled());
        let rx = handle.install_test_channel(); // a lazy respawn fills the shared tx
        assert!(
            cached.is_enabled(),
            "the pre-respawn clone must see the swapped-in channel"
        );
        cached.frame(AudioFrame::default());
        assert_eq!(
            drain_frames(&rx).len(),
            1,
            "frames from the pre-respawn clone reach the live channel"
        );
    }
}

/// The painters' handle — clone-cheap, non-blocking.
#[derive(Debug, Clone)]
pub(crate) struct AudioHandle {
    /// The live device sender, swappable IN PLACE behind a shared cell: every
    /// clone shares this `Arc`, so a consumer's cached clone survives a lazy
    /// respawn. `None` = disabled (no device, or sound not requested yet).
    tx: std::sync::Arc<std::sync::Mutex<Option<mpsc::SyncSender<AudioFrame>>>>,
    /// Mute is STATE, not an event: it rides this atomic instead of the
    /// droppable frame channel, so an `m`/`p` keypress lands even when a
    /// `try_send` drops.
    muted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Master volume (f32 bits) — same state-not-event rationale as `muted`.
    volume: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// The device thread's join handle, so [`shutdown`](Self::shutdown) can WAIT
    /// for `run_loop` to drop its `RodioSink` (the OS device close) before the
    /// process exits: detached, its teardown races exit, and on macOS CoreAudio a
    /// half-closed output strands playback (`sudo killall coreaudiod`).
    join: std::sync::Arc<std::sync::Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl AudioHandle {
    /// The inert handle: sound not requested yet, or no usable output device.
    pub(crate) fn disabled() -> Self {
        Self {
            tx: std::sync::Arc::new(std::sync::Mutex::new(None)),
            muted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            volume: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
            join: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Push one frame of audio intent. `try_send` — a saturated audio thread
    /// drops frames rather than ever stalling the render loop. A disabled handle drops
    /// it, so callers need no `is_enabled` guard.
    pub(crate) fn frame(&self, frame: AudioFrame) {
        if let Some(tx) = self.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.try_send(frame);
        }
    }

    pub(crate) fn set_muted(&self, muted: bool) {
        self.muted
            .store(muted, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        self.volume.store(
            volume.clamp(0.0, 1.0).to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub(crate) fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The EFFECTIVE silence state — the m-toggle OR'd with pause, since the
    /// caller stores the combined value here.
    pub(crate) fn is_muted(&self) -> bool {
        self.muted.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The ONE audibility predicate both painters' footer ♩ indicators read.
    pub(crate) fn is_audible(&self) -> bool {
        self.is_enabled() && !self.is_muted() && self.volume() > 0.0
    }

    /// (Re)open the output device + audio thread and swap the live sender INTO
    /// this handle in place, so every consumer that cached a clone keeps
    /// working. A no-device / feature-off system leaves the handle disabled.
    pub(crate) fn respawn_in_place(&self, volume: f32) {
        self.set_volume(volume);
        #[cfg(feature = "audio")]
        {
            let Some(device) = sink::rodio_sink::RodioSink::open() else {
                return;
            };
            let (tx, rx) = mpsc::sync_channel(64);
            let muted_for_loop = std::sync::Arc::clone(&self.muted);
            let vol_for_loop = std::sync::Arc::clone(&self.volume);
            let seed = pixtuoid_scene::audio::music_seed(std::time::SystemTime::now());
            match std::thread::Builder::new()
                .name("pixtuoid-audio".into())
                .spawn(move || {
                    // It feeds the music, which drops out once a render runs
                    // `MUSIC_AHEAD` late, while agents load the machine. Our
                    // inference from Apple's QoS table: user-initiated, "Work
                    // that the user has initiated and requires immediate
                    // results", not utility, "Work that may take some time to
                    // complete and doesn't require an immediate result"
                    // (<https://developer.apple.com/library/archive/documentation/Performance/Conceptual/EnergyGuide-iOS/PrioritizeWorkWithQoS.html>).
                    // The device plays on its output callback's own thread.
                    #[cfg(target_os = "macos")]
                    promote_to_user_initiated();
                    run_loop(rx, Box::new(device), muted_for_loop, vol_for_loop, seed)
                }) {
                Ok(join) => {
                    // Replacing the sole sender CLOSES a prior thread's channel — retire that
                    // thread rather than leak one still holding the output.
                    *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
                    let prior = self
                        .join
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(join);
                    // Both locks are dropped before the join: the keypress path must never
                    // block holding one.
                    if let Some(prior) = prior {
                        join_with_timeout(prior, SHUTDOWN_JOIN_TIMEOUT);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "audio: thread spawn failed, running silent"),
            }
        }
    }

    /// Stop the device thread SYNCHRONOUSLY: dropping the sole sender makes `run_loop` return
    /// and drop its `RodioSink` (the OS device close); the JOIN makes that Drop finish before
    /// the process exits (see `join`). INVARIANT: only from `AudioController::stop`, after the
    /// painter's loop ended — nothing races `tx`/`join`.
    pub(crate) fn shutdown(&self) {
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let handle = self.join.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            join_with_timeout(handle, SHUTDOWN_JOIN_TIMEOUT);
        }
    }

    /// Test seam: a live handle whose receiver the test drains, to see what the
    /// render path feeds the audio thread.
    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, mpsc::Receiver<AudioFrame>) {
        let (tx, rx) = mpsc::sync_channel(256);
        (
            Self {
                tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
                muted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                volume: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits())),
                join: std::sync::Arc::new(std::sync::Mutex::new(None)),
            },
            rx,
        )
    }

    /// Test seam: fill the shared sender in place, as a lazy respawn would.
    #[cfg(test)]
    pub(crate) fn install_test_channel(&self) -> mpsc::Receiver<AudioFrame> {
        let (tx, rx) = mpsc::sync_channel(256);
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }
}

#[cfg(test)]
const FAKE_TEARDOWN_MS: u64 = 300;

/// Install a fake device thread on `handle` that mirrors `run_loop` (block on the channel,
/// exit on disconnect) but tears down MEASURABLY slowly: a zero-cost teardown lets an
/// un-joined thread pass a join assert by luck. The returned flag flips once teardown ends.
#[cfg(test)]
fn install_slow_fake_device(handle: &AudioHandle) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let rx = handle.install_test_channel();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    let thread = std::thread::spawn(move || {
        while rx.recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(FAKE_TEARDOWN_MS));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    *handle.join.lock().unwrap() = Some(thread);
    done
}

#[cfg(test)]
pub(crate) fn drain_frames(rx: &mpsc::Receiver<AudioFrame>) -> Vec<AudioFrame> {
    let mut out = Vec::new();
    while let Ok(f) = rx.try_recv() {
        out.push(f);
    }
    out
}

/// How often the audio thread wakes to ramp gains / run schedulers when no
/// frames arrive (frames themselves also wake it).
#[cfg(feature = "audio")]
const TICK_MS: u64 = 50;

/// Upper bound on [`AudioHandle::shutdown`]'s wait for the device thread's teardown. The
/// common case returns within one [`TICK_MS`], but `run_loop` is BLIND to the closed channel
/// while it synthesizes the one-shots and rain at spawn, so a quit landing there must wait
/// that build out: a ceiling under its worst case times the join out and leaves the thread
/// DETACHED, the very failure the join exists to prevent.
const SHUTDOWN_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Join `handle`, but give up after `timeout` so a hung device-close (or a
/// still-in-flight synth build, see [`SHUTDOWN_JOIN_TIMEOUT`]) can't block the
/// exit forever. On timeout the helper thread is left detached, still blocked on
/// the real join.
/// std has no timed `JoinHandle::join`, hence the channel dance.
fn join_with_timeout(handle: std::thread::JoinHandle<()>, timeout: std::time::Duration) {
    let (done_tx, done_rx) = mpsc::channel();
    // `Builder::spawn` (not `thread::spawn`) so an OS thread-exhaustion failure returns `Err`
    // instead of PANICKING: this can run during unwind, where a panic double-panics → abort.
    let spawned = std::thread::Builder::new().spawn(move || {
        let _ = handle.join();
        let _ = done_tx.send(());
    });
    if spawned.is_ok() {
        let _ = done_rx.recv_timeout(timeout);
    }
}

/// The production spawn every caller injects — a named fn so they can't drift.
pub(crate) fn respawn(handle: &AudioHandle, volume: f32) {
    handle.respawn_in_place(volume);
}

/// Raise the calling thread to user-initiated QoS, Chromium's `kDefault`
/// (`base/threading/platform_thread_apple.mm`). A new thread reads back
/// default QoS (measured on an M1 Max), which "falls between user-initiated
/// and utility"
/// (<https://developer.apple.com/library/archive/documentation/Performance/Conceptual/EnergyGuide-iOS/PrioritizeWorkWithQoS.html>).
/// Elsewhere `kDefault` is no raise: Windows' is `THREAD_PRIORITY_NORMAL`
/// (`platform_thread_win.cc`), which "All threads are created using"
/// (<https://learn.microsoft.com/en-us/windows/win32/procthread/scheduling-priorities>),
/// and Linux's is nice 0 (`platform_thread_linux.cc`), which an unprivileged
/// thread lowers its nice to only under a suitable `RLIMIT_NICE`
/// (setpriority(2)). Best effort: a refusal leaves the thread as it was.
#[cfg(target_os = "macos")]
pub fn promote_to_user_initiated() {
    // SAFETY: sets the calling thread's own QoS class; nothing is borrowed.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod thread_class_tests {
    /// A promoted thread reads back user-initiated, from a class below it,
    /// and the thread that spawned it keeps its own.
    #[test]
    fn a_promoted_thread_reads_back_the_user_initiated_class() {
        let before = class_of_this_thread();
        let after = std::thread::spawn(|| {
            // SAFETY: sets the calling thread's own QoS class.
            let ok = unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0)
            };
            assert_eq!(ok, 0);
            assert_ne!(class_of_this_thread(), user_initiated(), "premise: lowered");
            super::promote_to_user_initiated();
            class_of_this_thread()
        })
        .join()
        .expect("the thread ran");
        assert_eq!(after, user_initiated(), "the promoted thread");
        assert_eq!(class_of_this_thread(), before, "only that thread");
    }

    fn class_of_this_thread() -> i64 {
        let (mut class, mut relative) = (libc::qos_class_t::QOS_CLASS_UNSPECIFIED, 0);
        // SAFETY: reads the calling thread's QoS into two locals.
        let ok = unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative)
        };
        assert_eq!(ok, 0);
        class as i64
    }

    fn user_initiated() -> i64 {
        libc::qos_class_t::QOS_CLASS_USER_INITIATED as i64
    }
}

/// The per-tick dt, CLAMPED to `MAX_DT_S` — the shell's half of the engine's
/// gap-immunity (the wasm painter clamps its `now_ms` delta the same way). A
/// scheduler-starvation gap must not cover seconds and snap a ramp or burst
/// the schedulers.
#[cfg(feature = "audio")]
fn clamped_dt(prev: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(prev)
        .as_secs_f32()
        .min(MAX_DT_S)
}

/// The audio thread body — the DEVICE shell over the shared [`AudioEngine`],
/// which owns all mixing/scheduling. `seed` picks the run of songs.
#[cfg(feature = "audio")]
fn run_loop(
    rx: mpsc::Receiver<AudioFrame>,
    mut device: Box<dyn AudioSink>,
    muted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    volume: std::sync::Arc<std::sync::atomic::AtomicU32>,
    seed: u64,
) {
    use std::sync::atomic::Ordering::Relaxed;

    let built_at = Instant::now();
    let mut synthesis = Synthesis::new();
    let bank = synthesis.assets();
    device.start_loop(LoopStem::Rain, synthesis.rain());
    let (mut music, reader) = music::stream(seed);
    device.start_music(reader);
    tracing::debug!(
        ms = built_at.elapsed().as_millis(),
        "audio: one-shots + rain synthesized"
    );

    let mut engine = AudioEngine::new(f32::from_bits(volume.load(Relaxed)));
    let mut last_step = Instant::now();

    loop {
        let msg = rx.recv_timeout(std::time::Duration::from_millis(TICK_MS));
        engine.set_muted(muted.load(Relaxed));
        engine.set_master(f32::from_bits(volume.load(Relaxed)));

        let now = Instant::now();
        let dt = clamped_dt(last_step, now);
        last_step = now;

        let frame = match msg {
            Ok(frame) => Some(frame),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };

        let heard = play(engine.tick(dt, frame), &mut *device, &mut music, &bank);
        if heard > 0 {
            tracing::debug!(frames = heard, "audio: the music underran");
        }
    }
}

/// Flush one tick's commands to `device` and the `music`; returns the music
/// frames the listener heard as silence since the last tick.
#[cfg(feature = "audio")]
fn play(
    cmds: pixtuoid_scene::audio::TickCommands,
    device: &mut dyn AudioSink,
    music: &mut music::MusicProducer,
    bank: &pixtuoid_scene::audio::bank::AssetBank,
) -> u64 {
    for (stem, gain) in LoopStem::ALL.into_iter().zip(cmds.gains) {
        device.set_loop_gain(stem, gain);
    }
    for play in cmds.plays {
        device.play_once(bank.sample(play.pool, play.index), play.gain);
    }
    music.tick(cmds.music)
}

#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;
    use pixtuoid_scene::audio::StemLevels;
    use std::sync::Arc;

    #[test]
    fn run_loop_starts_rain_and_music_plays_events_and_exits_on_disconnect() {
        let (tx, rx) = mpsc::sync_channel(8);
        // A `(Mutex, Condvar)` pair so the frame-1 barrier below BLOCKS on the sink's own
        // progress: an in-test deadline is the one flakiness knob nextest cannot reach.
        let recorder = Arc::new((
            std::sync::Mutex::new(sink::NullSink::default()),
            std::sync::Condvar::new(),
        ));
        // `.1` flips on the device's Drop, which the outliving recorder cannot observe.
        struct Probe(
            Arc<(std::sync::Mutex<sink::NullSink>, std::sync::Condvar)>,
            Arc<std::sync::atomic::AtomicBool>,
        );
        impl Probe {
            fn record(&self, f: impl FnOnce(&mut sink::NullSink)) {
                let (lock, progress) = &*self.0;
                f(&mut lock.lock().unwrap());
                progress.notify_all();
            }
        }
        impl AudioSink for Probe {
            fn start_loop(&mut self, stem: LoopStem, s: Arc<Vec<f32>>) {
                self.record(|r| r.start_loop(stem, s));
            }
            fn start_music(&mut self, reader: music::MusicReader) {
                self.record(|r| r.start_music(reader));
            }
            fn set_loop_gain(&mut self, stem: LoopStem, g: f32) {
                self.record(|r| r.set_loop_gain(stem, g));
            }
            fn play_once(&mut self, s: Arc<Vec<f32>>, g: f32) {
                self.record(|r| r.play_once(s, g));
            }
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                self.1.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let device_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = Probe(Arc::clone(&recorder), Arc::clone(&device_dropped));
        let muted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let muted_ctl = std::sync::Arc::clone(&muted);
        let vol = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits()));
        let join = std::thread::spawn(move || run_loop(rx, Box::new(probe), muted, vol, 7));

        // Rain stays 0 so no scheduler one-shot can race the count — only the
        // two frame events are audible.
        tx.send(AudioFrame {
            stems: StemLevels::default(),
            events: vec![OneShot::DoorChime, OneShot::PrinterWhir],
            music: Default::default(),
        })
        .unwrap();
        // Wait out frame 1 (the spawn-time bank build delays it) so the mute lands BETWEEN
        // the frames. UNBOUNDED on purpose: nextest's `terminate-after` names a stuck loop.
        {
            let (lock, progress) = &*recorder;
            let mut rec = lock.lock().unwrap();
            while rec.one_shots < 2 {
                rec = progress.wait(rec).unwrap();
            }
        }
        muted_ctl.store(true, std::sync::atomic::Ordering::Relaxed);
        tx.send(AudioFrame {
            stems: StemLevels::default(),
            events: vec![OneShot::DoorChime, OneShot::VendingDrop],
            music: Default::default(),
        })
        .unwrap();
        drop(tx);
        join.join().unwrap();

        assert!(
            device_dropped.load(std::sync::atomic::Ordering::SeqCst),
            "run_loop must DROP its device when the channel closes — that Drop is \
             the RodioSink closing the OS output; a refactor that leaked it would \
             re-strand audio on quit (the bug shutdown()'s join exists to force)"
        );

        let rec = recorder.0.lock().unwrap();
        assert_eq!(
            rec.loops_started,
            [LoopStem::Rain],
            "rain registers at spawn"
        );
        assert!(
            rec.music.as_ref().is_some_and(|m| m.queued() > 0),
            "the music starts at spawn and renders once a frame asks for it"
        );
        assert_eq!(
            rec.one_shots, 2,
            "the unmuted frame's 2 events played; the post-mute frame's 2 did not"
        );
    }

    #[test]
    fn clamped_dt_caps_a_stall_gap_but_passes_a_normal_tick() {
        let t0 = Instant::now();
        assert_eq!(
            clamped_dt(t0, t0 + std::time::Duration::from_secs(2)),
            MAX_DT_S,
            "a multi-second stall clamps to the ceiling"
        );
        let dt = clamped_dt(t0, t0 + std::time::Duration::from_millis(20));
        assert!(
            (dt - 0.020).abs() < 1e-4,
            "a normal tick passes through: {dt}"
        );
    }
}

/// The LISTEN gate: renders the office at each busy-ness tier, across dusk and
/// in rain, through the REAL mixer/schedulers/synth, the music's ring and the
/// device's [`sink::limited`] output into wav files for the owner's audition.
/// `#[ignore]` — run explicitly:
/// `just test -p pixtuoid --lib audio::listen_gate --run-ignored only --no-capture`
#[cfg(all(test, feature = "audio"))]
mod listen_gate {
    use super::*;
    use pixtuoid_scene::audio::bank::AssetBank;
    use pixtuoid_scene::audio::{SAMPLE_RATE, music_controls, stem_levels};
    use std::io::Write;
    use std::sync::Arc;

    /// Sample-accurate mixdown of loops, one-shots and the music into one
    /// master buffer, as a mono device hears it: the music folded as
    /// `MusicSource` folds it on a 1-channel bus.
    struct OfflineSink {
        master: Vec<f32>,
        loops: Vec<(Arc<Vec<f32>>, f32)>, // (samples, current gain)
        loop_ids: Vec<LoopStem>,
        music: Option<music::MusicReader>,
        write_pos_samples: usize,
    }

    impl OfflineSink {
        fn new(secs: f32) -> Self {
            Self {
                master: vec![0.0; (secs * SAMPLE_RATE as f32) as usize],
                loops: Vec::new(),
                loop_ids: Vec::new(),
                music: None,
                write_pos_samples: 0,
            }
        }

        fn advance(&mut self, n: usize) {
            for i in 0..n {
                let at = self.write_pos_samples + i;
                if at >= self.master.len() {
                    return;
                }
                for (samples, gain) in &self.loops {
                    self.master[at] += samples[at % samples.len()] * gain;
                }
                if let Some(music) = &mut self.music {
                    self.master[at] += music::fold(music.next_frame());
                }
            }
            self.write_pos_samples += n;
        }
    }

    impl AudioSink for OfflineSink {
        fn start_loop(&mut self, stem: LoopStem, samples: Arc<Vec<f32>>) {
            self.loops.push((samples, 0.0));
            self.loop_ids.push(stem);
        }
        fn start_music(&mut self, reader: music::MusicReader) {
            self.music = Some(reader);
        }
        fn set_loop_gain(&mut self, stem: LoopStem, gain: f32) {
            if let Some(i) = self.loop_ids.iter().position(|s| *s == stem) {
                self.loops[i].1 = gain;
            }
        }
        fn play_once(&mut self, samples: Arc<Vec<f32>>, gain: f32) {
            for (i, &s) in samples.iter().enumerate() {
                if let Some(slot) = self.master.get_mut(self.write_pos_samples + i) {
                    *slot += s * gain;
                }
            }
        }
    }

    fn write_wav(path: &std::path::Path, samples: &[f32]) {
        let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
        let data_len = (samples.len() * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for &s in samples {
            let clipped = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&clipped.to_le_bytes());
        }
        std::fs::File::create(path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    }

    /// One audition: an office of `active` agents under `rain`, its sky's
    /// nightfall at each second, its one-shots, and its length.
    struct Row<'a> {
        name: &'a str,
        active: usize,
        rain: f32,
        nightfall: fn(f32) -> f32,
        events_at: &'a [(f32, OneShot)],
        secs: f32,
    }

    /// The seed every row plays, so the rows differ only in their office.
    const SEED: u64 = 0;

    fn render(bank: &AssetBank, rain_bed: &Arc<Vec<f32>>, row: &Row) -> Vec<f32> {
        let counts = pixtuoid_scene::tally::StateCounts {
            active: row.active,
            waiting: 0,
            idle: 0,
            exiting: 0,
            total: row.active,
        };
        let mut sink = OfflineSink::new(row.secs);
        sink.start_loop(LoopStem::Rain, Arc::clone(rain_bed));
        let (mut music, reader) = music::stream(SEED);
        sink.start_music(reader);
        // Drive the SAME shared `AudioEngine` the app runs, so the audition
        // mixes exactly what ships (incl. the production bus trim).
        let mut engine = AudioEngine::new(1.0);
        let step_s = 0.05f32;
        let step_n = (step_s * SAMPLE_RATE as f32) as usize;
        let mut fired = vec![false; row.events_at.len()];
        let mut now_s = 0.0f32;
        while now_s < row.secs {
            let mut events = Vec::new();
            for (i, (at, ev)) in row.events_at.iter().enumerate() {
                if !fired[i] && now_s >= *at {
                    fired[i] = true;
                    events.push(*ev);
                }
            }
            let frame = AudioFrame {
                stems: stem_levels(&counts, row.rain),
                events,
                music: music_controls(&counts, (row.nightfall)(now_s), row.rain),
            };
            play(
                engine.tick(step_s, Some(frame)),
                &mut sink,
                &mut music,
                bank,
            );
            sink.advance(step_n);
            now_s += step_s;
        }
        sink::limited(sink::mono(&sink.master)).collect()
    }

    #[test]
    #[ignore = "the LISTEN gate: renders audition wavs for the owner's ears"]
    fn render_listen_gate_wavs() {
        let out = std::env::temp_dir().join("pixtuoid-audio-audition");
        std::fs::create_dir_all(&out).unwrap();
        let mut synthesis = Synthesis::new();
        let bank = synthesis.assets();
        let rain_bed = synthesis.rain();
        let volley = [
            (5.0, OneShot::DoorChime),
            (10.0, OneShot::PrinterWhir),
            (15.0, OneShot::VendingDrop),
        ];
        // The limiter's worst case: every appliance at once, over and over, on
        // the busiest tier in a storm.
        let pileup: Vec<(f32, OneShot)> = (0..8u8)
            .flat_map(|k| {
                let at = 5.0 + f32::from(k) * 1.5;
                [
                    OneShot::DoorChime,
                    OneShot::PrinterWhir,
                    OneShot::VendingDrop,
                ]
                .map(|e| (at, e))
            })
            .collect();
        let day: fn(f32) -> f32 = |_| 0.0;
        let night: fn(f32) -> f32 = |_| 1.0;
        // Through the first minute, so the next song is the night family's.
        let dusk: fn(f32) -> f32 = |t| (t / 60.0).min(1.0);
        let row = |name, active, rain, nightfall, events_at, secs| Row {
            name,
            active,
            rain,
            nightfall,
            events_at,
            secs,
        };
        for row in [
            row("tier_1_empty", 0, 0.0, day, &[][..], 60.0),
            row("tier_2_moderate", 1, 0.0, day, &[][..], 60.0),
            row("tier_3_busy_oneshot_volley", 3, 0.0, day, &volley[..], 60.0),
            row("tier_4_rainy_busy", 3, 1.0, day, &[][..], 60.0),
            row("tier_5_rainy_busy_pileup", 3, 1.0, day, &pileup[..], 60.0),
            row("dusk_moderate", 1, 0.0, dusk, &[][..], 240.0),
            // The rain stem over the music the rain darkens, at night.
            row("night_rain_moderate", 1, 1.0, night, &[][..], 120.0),
        ] {
            let buf = render(&bank, &rain_bed, &row);
            assert!(
                buf.iter().any(|&s| s.abs() > 0.01),
                "{}: every row is audible",
                row.name
            );
            write_wav(&out.join(format!("{}.wav", row.name)), &buf);
        }
        println!("LISTEN GATE wavs at: {}", out.display());
    }
}
