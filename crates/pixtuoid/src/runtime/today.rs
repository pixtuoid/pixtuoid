//! The day's count across runs, in one state-directory file every instance
//! writes. Each instance folds the same events through its own reducer, so a
//! write keeps the larger of the file's number and its own: a day's counters only
//! rise, so nothing double-counts and no instance is special. One started late
//! errs low.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use chrono::NaiveDate;
use pixtuoid_core::SceneState;
use pixtuoid_core::state::Today;
use pixtuoid_scene::tally::local_day;

use crate::install::io::{lock_config, read_config};

/// The least time between two writes of a changing count.
const SAVE_EVERY: Duration = Duration::from_secs(60);

/// How a reducer keeps the day's count.
pub(crate) struct DayCount {
    /// What dates the count, a test's own day keeping it off the runner's zone and clock.
    pub(crate) day_of: fn(SystemTime) -> NaiveDate,
    pub(crate) file: Option<TodayFile>,
}

impl DayCount {
    pub(crate) fn local() -> Self {
        Self {
            day_of: local_day,
            file: TodayFile::in_state_dir(),
        }
    }
}

/// The file behind the count.
#[derive(Debug, Clone)]
pub(crate) struct TodayFile(PathBuf);

impl TodayFile {
    fn in_state_dir() -> Option<Self> {
        Some(Self(crate::config::state_dir()?.join("today.json")))
    }

    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self(path)
    }

    /// `None` when it can't be read: a count is no reason to fail a boot.
    fn parse(text: &str) -> Option<Today> {
        if text.is_empty() {
            return None;
        }
        serde_json::from_str(text)
            .inspect_err(|error| tracing::warn!(error = ?error, "unreadable saved count"))
            .ok()
    }

    /// What an earlier run counted on `day`. Unlocked: a write is an atomic
    /// rename, so a reader sees a whole file.
    fn load(&self, day: NaiveDate) -> Option<Today> {
        let text = read_config(&self.0)
            .inspect_err(|error| tracing::warn!(error = ?error, "cannot read the saved count"))
            .ok()?;
        Self::parse(&text).filter(|today| today.day == day)
    }

    /// One locked read→merge→write round. Fails on contention, which the next
    /// write makes up for.
    fn save(&self, own: Today) -> Result<()> {
        let lock = lock_config(&self.0)?;
        let saved = Self::parse(&lock.read()?);
        lock.write_atomic(&serde_json::to_string(&merged(saved, own))?)
    }
}

/// The file's next count: the later day's, or each number's larger on one day.
fn merged(saved: Option<Today>, own: Today) -> Today {
    match saved {
        Some(saved) if saved.day > own.day => saved,
        Some(saved) if saved.day == own.day => Today::new(
            own.day,
            own.tokens.max(saved.tokens),
            own.tool_calls.max(saved.tool_calls),
        ),
        _ => own,
    }
}

/// When a changing count goes to its [`TodayFile`]. Its last write is its drop:
/// a quit drops the runtime, and the reducer's task with it, parked and past
/// reaching any line after its loop.
#[derive(Debug)]
pub(super) struct TodaySaver {
    file: TodayFile,
    /// The count as of the reducer's last event.
    latest: Option<Today>,
    /// What the file was last known to take, set once a write lands.
    saved: Arc<Mutex<Option<Today>>>,
    tried_at: Option<Instant>,
}

impl TodaySaver {
    /// A saver over `file`, setting what it holds for `day` on the boot `scene`.
    pub(super) fn boot(file: TodayFile, scene: &mut SceneState, day: NaiveDate) -> Self {
        scene.today = file.load(day);
        Self {
            file,
            latest: scene.today,
            saved: Arc::new(Mutex::new(scene.today)),
            tried_at: None,
        }
    }

    pub(super) fn note(&mut self, today: Option<Today>) {
        self.latest = today;
    }

    /// The count to write `now`: an unsaved one, [`SAVE_EVERY`] past the last try.
    /// A write in flight makes none due, since this runs on the reducer's task,
    /// which a hung disk must not stall.
    fn due(&mut self, now: Instant) -> Option<Today> {
        if (self.tried_at).is_some_and(|at| now.duration_since(at) < SAVE_EVERY) {
            return None;
        }
        let saved = match self.saved.try_lock() {
            Ok(saved) => *saved,
            Err(TryLockError::Poisoned(saved)) => *saved.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        let today = self.latest.filter(|today| Some(*today) != saved)?;
        self.tried_at = Some(now);
        Some(today)
    }

    /// Holds `saved` throughout: the file's lock fails on contention, and the
    /// last write must wait out a tick's rather than lose to it.
    fn write(file: &TodayFile, saved: &Mutex<Option<Today>>, today: Today) {
        let mut saved = saved.lock().unwrap_or_else(PoisonError::into_inner);
        match file.save(today) {
            Ok(()) => *saved = Some(today),
            Err(error) => tracing::warn!(error = ?error, "cannot save the day's count"),
        }
    }

    /// Off the reducer's task: a write fsyncs.
    pub(super) fn tick(&mut self, today: Option<Today>, now: Instant) {
        self.note(today);
        if let Some(today) = self.due(now) {
            let (file, saved) = (self.file.clone(), Arc::clone(&self.saved));
            tokio::task::spawn_blocking(move || Self::write(&file, &saved, today));
        }
    }
}

impl Drop for TodaySaver {
    fn drop(&mut self) {
        let saved = *self.saved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(today) = self.latest.filter(|today| Some(*today) != saved) {
            Self::write(&self.file, &self.saved, today);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2027, 1, 15).expect("a date")
    }

    fn next_day() -> NaiveDate {
        day().succ_opt().expect("a date")
    }

    fn fixture() -> (tempfile::TempDir, TodayFile) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = TodayFile::at(dir.path().join("pixtuoid").join("today.json"));
        (dir, file)
    }

    fn saver(file: TodayFile, saved: Option<Today>) -> TodaySaver {
        TodaySaver {
            file,
            latest: saved,
            saved: Arc::new(Mutex::new(saved)),
            tried_at: None,
        }
    }

    #[test]
    fn a_saved_count_is_loaded_on_its_own_day_only() {
        let (_dir, file) = fixture();
        assert_eq!(file.load(day()), None, "nothing saved yet");
        let today = Today::new(day(), 1_200_000, 38);
        file.save(today).expect("it saves");
        assert_eq!(file.load(day()), Some(today));
        assert_eq!(
            file.load(next_day()),
            None,
            "yesterday's is no count of today"
        );
    }

    /// Every instance folds the same stream, so however their writes interleave the
    /// file holds the most any saw, of the latest day, and never their sum.
    #[test]
    fn two_instances_count_one_day_once() {
        let at = Today::new;
        let (one, two) = (day(), next_day());
        let rows: [(&str, &[Today], Today); 4] = [
            (
                "each number's larger of the file's and its own",
                &[at(one, 100, 1), at(one, 50, 2)],
                at(one, 100, 2),
            ),
            (
                "the same count from each",
                &[
                    at(one, 10, 0),
                    at(one, 10, 0),
                    at(one, 20, 0),
                    at(one, 15, 0),
                ],
                at(one, 20, 0),
            ),
            (
                "a new day replaces the last",
                &[at(one, 100, 10), at(two, 5, 0)],
                at(two, 5, 0),
            ),
            (
                "an instance no event carried past midnight leaves today alone",
                &[at(two, 5, 0), at(one, 100, 10)],
                at(two, 5, 0),
            ),
        ];
        for (row, writes, holds) in rows {
            let (_dir, file) = fixture();
            for write in writes {
                file.save(*write).expect("it saves");
            }
            assert_eq!(file.load(holds.day), Some(holds), "{row}");
        }
    }

    #[test]
    fn an_unreadable_file_loads_nothing_and_the_next_write_replaces_it() {
        let (dir, file) = fixture();
        std::fs::create_dir_all(dir.path().join("pixtuoid")).expect("it exists");
        std::fs::write(&file.0, "{not json").expect("it writes");
        assert_eq!(file.load(day()), None);
        let today = Today::new(day(), 7, 0);
        file.save(today)
            .expect("a broken count stands in no one's way");
        assert_eq!(file.load(day()), Some(today));
    }

    #[test]
    fn the_file_sits_in_the_state_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        temp_env::with_var("XDG_STATE_HOME", Some(dir.path()), || {
            let file = TodayFile::in_state_dir().expect("a state directory");
            assert_eq!(file.0, dir.path().join("pixtuoid").join("today.json"));
        });
    }

    #[test]
    fn a_change_saves_at_once_then_at_most_once_a_period() {
        let (_dir, file) = fixture();
        let start = Instant::now();
        let count = |tokens| Some(Today::new(day(), tokens, 0));
        let mut saver = saver(file, None);
        assert_eq!(saver.due(start), None, "nothing to save");
        saver.note(count(1));
        assert_eq!(saver.due(start), count(1), "the first change");
        saver.note(count(2));
        let inside = start + SAVE_EVERY - Duration::from_millis(1);
        assert_eq!(saver.due(inside), None, "inside the period");
        assert_eq!(saver.due(start + SAVE_EVERY), count(2));
    }

    #[test]
    fn a_failed_write_is_tried_again_and_a_landed_one_is_not() {
        let (_dir, file) = fixture();
        let start = Instant::now();
        let today = Today::new(day(), 1, 0);
        let mut saver = saver(file.clone(), None);
        saver.note(Some(today));

        let lock = lock_config(&file.0).expect("the lock");
        let first = saver.due(start).expect("due");
        TodaySaver::write(&saver.file, &saver.saved, first);
        drop(lock);
        assert_eq!(file.load(day()), None, "the write failed");

        let again = saver.due(start + SAVE_EVERY).expect("due again");
        TodaySaver::write(&saver.file, &saver.saved, again);
        assert_eq!(file.load(day()), Some(today));
        assert_eq!(saver.due(start + 2 * SAVE_EVERY), None, "it landed");
    }

    #[test]
    fn a_write_in_flight_never_holds_up_the_reducers_task() {
        let (_dir, file) = fixture();
        let start = Instant::now();
        let today = Today::new(day(), 1, 0);
        let mut saver = saver(file, None);
        saver.note(Some(today));
        let in_flight = Arc::clone(&saver.saved);
        let held = in_flight.lock().expect("the lock");

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let due = saver.due(start);
            let _ = done.send((due, saver));
        });
        let (due, mut saver) = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("due returned with the lock held");
        assert_eq!(due, None);

        drop(held);
        assert_eq!(saver.due(start), Some(today), "no try was spent on it");
    }
}
