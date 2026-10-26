//! macOS's watcher of a directory's entries — every anchor, and a
//! non-recursive root — over a [`VnodeWatch`] of the directory itself.

use std::path::Path;

use notify::event::{EventKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Config, EventHandler, RecursiveMode, Watcher, WatcherKind};
use rustix::event::kqueue::VnodeEvents;

use crate::source::kqueue::{VnodeNote, VnodeWatch};

/// The directory's entry list changing, or the directory itself going away.
const WATCHED: VnodeEvents = VnodeEvents::WRITE
    .union(VnodeEvents::LINK)
    .union(VnodeEvents::DELETE)
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE);

/// Watches each directory a [`Watcher::watch`] call names, non-recursively.
pub(super) struct KqueueDirWatcher(VnodeWatch);

fn kind_of(flags: VnodeEvents) -> EventKind {
    if flags.intersects(VnodeEvents::DELETE | VnodeEvents::REVOKE) {
        EventKind::Remove(RemoveKind::Folder)
    } else if flags.contains(VnodeEvents::RENAME) {
        EventKind::Modify(ModifyKind::Name(RenameMode::Any))
    } else {
        // kqueue says only that an entry changed, not which.
        EventKind::Any
    }
}

impl Watcher for KqueueDirWatcher {
    fn new<F: EventHandler>(mut handler: F, _config: Config) -> notify::Result<Self> {
        let watch = VnodeWatch::spawn("pixtuoid-dir-watch", move |_, note| match note {
            VnodeNote::Changed(path, flags) => handler
                .handle_event(Ok(
                    notify::Event::new(kind_of(flags)).add_path(path.to_path_buf())
                )),
            VnodeNote::Failed(e) => handler.handle_event(Err(notify::Error::io(e))),
        })
        .map_err(notify::Error::io)?;
        Ok(Self(watch))
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        if recursive_mode == RecursiveMode::Recursive {
            return Err(
                notify::Error::generic("a kqueue directory watch is non-recursive")
                    .add_path(path.to_path_buf()),
            );
        }
        self.0
            .vnodes()
            .add(path, WATCHED)
            .map(|_| ())
            .map_err(|e| notify::Error::io_watch(e).add_path(path.to_path_buf()))
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        if self.0.vnodes().remove(path) {
            Ok(())
        } else {
            Err(notify::Error::watch_not_found().add_path(path.to_path_buf()))
        }
    }

    fn kind() -> WatcherKind {
        WatcherKind::Kqueue
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);

    fn watcher() -> (KqueueDirWatcher, std::sync::mpsc::Receiver<notify::Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = KqueueDirWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let _ = tx.send(res.expect("a healthy kqueue reports no error"));
            },
            Config::default(),
        )
        .unwrap();
        (watcher, rx)
    }

    #[test]
    fn reports_an_entry_change_then_its_own_removal() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("anchor");
        std::fs::create_dir(&anchor).unwrap();
        let (mut watcher, rx) = watcher();
        watcher.watch(&anchor, RecursiveMode::NonRecursive).unwrap();

        std::fs::create_dir(anchor.join("child")).unwrap();
        let event = rx.recv_timeout(DEADLINE).expect("a new child is reported");
        assert_eq!(event.paths, vec![anchor.clone()]);
        assert_eq!(event.kind, EventKind::Any);

        std::fs::remove_dir(anchor.join("child")).unwrap();
        std::fs::remove_dir(&anchor).unwrap();
        let removed = std::iter::from_fn(|| rx.recv_timeout(DEADLINE).ok())
            .find(|e| matches!(e.kind, EventKind::Remove(_)));
        assert!(removed.is_some(), "the anchor's own removal is reported");
    }

    #[test]
    fn watches_a_directory_only_non_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let (mut watcher, _rx) = watcher();
        assert!(watcher.watch(dir.path(), RecursiveMode::Recursive).is_err());
        watcher
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .unwrap();
    }
}
