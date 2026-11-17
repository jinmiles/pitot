//! Updates of an installed Pitot, through the installer's library (Velopack): finds a newer release, downloads and
//! verifies it, and installs it when the window's button is pressed. Only the window's process runs this, on
//! threads of its own, so neither the overlay's process nor the window's UI thread waits on the network.
//!
//! The releases are the GitHub releases of [`RELEASES`], pre-releases included (every release before 1.0 is
//! one). [`SOURCE_VARIABLE`] points the updater elsewhere, for testing the whole flow without publishing.

use std::ffi::OsString;
use std::time::Duration;

use pitot_overlay::platform;
use velopack::sources::{AutoSource, GithubSource, UpdateSource};
use velopack::{UpdateCheck, UpdateInfo, UpdateManager};

use crate::model::Update;

/// The repository whose releases hold the installer and the update packages.
pub const RELEASES: &str = "https://github.com/jinmiles/pitot";

/// A developer option: a folder holding a release feed (`releases.win.json`) and its packages, or a URL of one,
/// used instead of [`RELEASES`].
pub const SOURCE_VARIABLE: &str = "PITOT_UPDATE_SOURCE";

/// How long the overlay gets to quit before the installer stops it.
const QUIT_TIMEOUT: Duration = Duration::from_secs(15);

/// The installed Pitot's updater. Cheap to clone; each clone shares the same installation.
#[derive(Clone)]
pub struct Updater {
    manager: UpdateManager,
}

/// What a check found.
pub enum Found {
    /// A newer version, to download.
    Newer(Box<UpdateInfo>),
    UpToDate,
    /// The releases list no version at all.
    Nothing,
}

/// News from an update thread for the window.
pub enum Event {
    /// What to show now.
    State(Update),
    /// A newer version, downloaded and verified: what the button installs.
    Ready(Box<UpdateInfo>),
    /// The installer starts once this process exits: close the window.
    Quit,
}

impl Updater {
    /// The installation this process runs from, with its updates from [`source`]. `None` when it does not run
    /// from an installation.
    pub fn installed() -> Option<Self> {
        UpdateManager::new_boxed(source(), None, None)
            .ok()
            .map(|manager| Self { manager })
    }

    /// The installed version.
    pub fn version(&self) -> String {
        self.manager.get_current_version_as_string()
    }

    /// Asks the releases for a newer version than the installed one. Only full releases count, compared as
    /// semantic versions, so a pre-release of a version is older than the version itself.
    pub fn check(&self) -> Result<Found, velopack::Error> {
        Ok(match self.manager.check_for_updates()? {
            UpdateCheck::UpdateAvailable(update) => Found::Newer(update),
            UpdateCheck::NoUpdateAvailable => Found::UpToDate,
            UpdateCheck::RemoteIsEmpty => Found::Nothing,
        })
    }

    /// Downloads `update` into the installation's packages, unless it is there already, and checks its size and
    /// SHA-256 against the release feed; a package that does not match is not kept. `progress` gets percents.
    pub fn download(
        &self,
        update: &UpdateInfo,
        mut progress: impl FnMut(u8) + Send,
    ) -> Result<(), velopack::Error> {
        let (sender, receiver) = std::sync::mpsc::channel::<i16>();
        std::thread::scope(|scope| {
            // The library reports progress through a channel from the downloading thread.
            scope.spawn(move || {
                for percent in receiver {
                    progress(percent.clamp(0, 100) as u8);
                }
            });
            self.manager.download_updates(update, Some(sender))
        })
    }

    /// Starts the installer for `update`, which waits for this process to exit, replaces the installation, and
    /// starts the overlay again. Nothing is shown meanwhile; the window shows what happens until it closes.
    fn start_install(&self, update: &UpdateInfo) -> Result<(), velopack::Error> {
        self.manager
            .wait_exit_then_apply_updates(update, true, true, Vec::<OsString>::new())
    }
}

/// Where updates come from: [`SOURCE_VARIABLE`] when it is set, else the GitHub releases of [`RELEASES`]. Only
/// requests the HTTP client needs are sent: the GitHub API's list of releases and the files in them, with the
/// client's own generic user agent.
fn source() -> Box<dyn UpdateSource> {
    match std::env::var(SOURCE_VARIABLE) {
        Ok(custom) if !custom.trim().is_empty() => Box::new(AutoSource::new(custom.trim())),
        _ => Box::new(GithubSource::new(RELEASES, None, true)),
    }
}

/// On a thread of its own: checks for a newer version and downloads it, telling `send` each step.
pub fn check_and_download(updater: Updater, send: impl Fn(Event) + Send + Sync + 'static) {
    spawn(move || {
        send(Event::State(Update::Checking));
        let update = match updater.check() {
            Ok(Found::Newer(update)) => update,
            Ok(Found::UpToDate) => return send(Event::State(Update::UpToDate)),
            Ok(Found::Nothing) => return send(Event::State(Update::NoneFound)),
            Err(_) => return send(Event::State(Update::CheckFailed)),
        };
        let version = update.TargetFullRelease.Version.clone();
        let downloading = |percent| {
            Event::State(Update::Downloading {
                version: version.clone(),
                percent,
            })
        };
        send(downloading(0));
        match updater.download(&update, |percent| send(downloading(percent))) {
            Ok(()) => send(Event::Ready(update)),
            Err(_) => send(Event::State(Update::DownloadFailed)),
        }
    });
}

/// On a thread of its own: quits the overlay the usual way, so it logs its counters, waits for it to end, and
/// starts the installer for `update`. Tells `send` to close the window once the installer waits for it; if the
/// installer cannot start, the overlay is started again.
pub fn install(updater: Updater, update: Box<UpdateInfo>, send: impl Fn(Event) + Send + 'static) {
    spawn(move || {
        let version = update.TargetFullRelease.Version.clone();
        send(Event::State(Update::Closing {
            version: version.clone(),
        }));
        let was_running = platform::overlay_running();
        // An overlay that does not quit in time is stopped by the installer.
        let _ = platform::quit_overlay_and_wait(QUIT_TIMEOUT);
        send(Event::State(Update::Installing { version }));
        match updater.start_install(&update) {
            Ok(()) => send(Event::Quit),
            Err(_) => {
                if was_running {
                    let _ = platform::start_overlay();
                }
                send(Event::State(Update::InstallFailed));
            }
        }
    });
}

fn spawn(work: impl FnOnce() + Send + 'static) {
    // A thread that cannot start leaves the window as it was.
    let _ = std::thread::Builder::new()
        .name("pitot-update".to_owned())
        .spawn(work);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use velopack::locator::create_config_from_root_dir;
    use velopack::sources::FileSource;

    use super::*;

    /// A fake installation of `version` in a folder of its own, with a release feed folder next to it.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, version: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("pitot-updates-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let current = dir.join("install").join("current");
            fs::create_dir_all(&current).expect("install dir");
            fs::create_dir_all(dir.join("feed")).expect("feed dir");
            // Only its existence is checked.
            fs::write(dir.join("install").join("Update.exe"), b"").expect("updater");
            let manifest = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package><metadata><id>Pitot</id>\
                 <version>{version}</version><title>Pitot</title><mainExe>pitot.exe</mainExe>\
                 <channel>win</channel></metadata></package>\n"
            );
            fs::write(current.join("sq.version"), manifest).expect("manifest");
            Self { dir }
        }

        fn feed(&self) -> PathBuf {
            self.dir.join("feed")
        }

        /// The feed lists `assets`: (version, type, file name, size, SHA-256).
        fn publish(&self, assets: &[(&str, &str, &str, u64, &str)]) {
            let assets: Vec<String> = assets
                .iter()
                .map(|(version, kind, file, size, sha256)| {
                    format!(
                        "{{\"PackageId\":\"Pitot\",\"Version\":\"{version}\",\"Type\":\"{kind}\",\
                         \"FileName\":\"{file}\",\"SHA1\":\"\",\"SHA256\":\"{sha256}\",\"Size\":{size}}}"
                    )
                })
                .collect();
            let feed = format!("{{\"Assets\":[{}]}}", assets.join(","));
            fs::write(self.feed().join("releases.win.json"), feed).expect("feed");
        }

        fn updater(&self) -> Updater {
            let config = create_config_from_root_dir(self.dir.join("install"));
            let manager = UpdateManager::new(FileSource::new(self.feed()), None, Some(config))
                .expect("a fake installation");
            Updater { manager }
        }

        fn packages(&self) -> PathBuf {
            self.dir.join("install").join("packages")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn newer(updater: &Updater) -> Option<String> {
        match updater.check().expect("the feed reads") {
            Found::Newer(update) => Some(update.TargetFullRelease.Version.clone()),
            Found::UpToDate => None,
            Found::Nothing => Some("nothing".to_owned()),
        }
    }

    fn full(
        version: &'static str,
    ) -> (&'static str, &'static str, &'static str, u64, &'static str) {
        (version, "Full", "unused.nupkg", 1, "")
    }

    #[test]
    fn a_copy_that_is_not_installed_has_no_updater() {
        // The test runs from cargo's target folder, as a build of Pitot does.
        assert!(Updater::installed().is_none());
    }

    #[test]
    fn newer_versions_and_pre_releases_compare_as_semantic_versions() {
        let fixture = Fixture::new("versions", "0.1.4");
        let updater = fixture.updater();
        assert_eq!(updater.version(), "0.1.4");

        fixture.publish(&[full("0.1.3"), full("0.1.4")]);
        assert_eq!(newer(&updater), None, "the same and older versions");
        fixture.publish(&[full("0.1.4"), full("0.1.5-beta.1")]);
        assert_eq!(
            newer(&updater).as_deref(),
            Some("0.1.5-beta.1"),
            "a pre-release of a later version is newer"
        );
        fixture.publish(&[full("0.1.4-rc.1")]);
        assert_eq!(
            newer(&updater),
            None,
            "a pre-release of the installed version is older"
        );
        fixture.publish(&[full("0.1.10"), full("0.1.9"), full("0.2.0-rc.1")]);
        assert_eq!(newer(&updater).as_deref(), Some("0.2.0-rc.1"), "the newest");
        fixture.publish(&[
            full("0.1.4"),
            ("0.1.6", "Delta", "Pitot-0.1.6-delta.nupkg", 1, ""),
        ]);
        assert_eq!(newer(&updater), None, "a delta alone is no release");
        fixture.publish(&[]);
        assert_eq!(newer(&updater).as_deref(), Some("nothing"));
    }

    #[test]
    fn pre_releases_compare_among_themselves() {
        let fixture = Fixture::new("pre", "0.1.5-test.1");
        let updater = fixture.updater();
        fixture.publish(&[full("0.1.5-test.1"), full("0.1.5-test.2")]);
        assert_eq!(newer(&updater).as_deref(), Some("0.1.5-test.2"));
        fixture.publish(&[full("0.1.5")]);
        assert_eq!(
            newer(&updater).as_deref(),
            Some("0.1.5"),
            "the release after its pre-releases"
        );
        fixture.publish(&[full("0.1.4")]);
        assert_eq!(newer(&updater), None, "never a downgrade");
    }

    #[test]
    fn a_download_must_match_the_feed() {
        const PACKAGE: &[u8] = b"not really a package";
        const SHA256: &str = "ad25bc0f144b930b9468833a0c80eaf41212e76ec10e170d7a7e16b6acff4078";
        const FILE: &str = "Pitot-0.1.5-full.nupkg";
        let fixture = Fixture::new("download", "0.1.4");
        fs::write(fixture.feed().join(FILE), PACKAGE).expect("package");
        let updater = fixture.updater();
        let kept = |fixture: &Fixture| Path::exists(&fixture.packages().join(FILE));
        let try_with = |size: u64, sha256: &str| {
            fixture.publish(&[("0.1.5", "Full", FILE, size, sha256)]);
            let Found::Newer(update) = updater.check().expect("checks") else {
                panic!("0.1.5 is newer");
            };
            let mut percents = Vec::new();
            let result = updater.download(&update, |p| percents.push(p));
            (result, percents)
        };

        let wrong = SHA256.replace('a', "b");
        let (result, _) = try_with(PACKAGE.len() as u64, &wrong);
        assert!(
            matches!(result, Err(velopack::Error::ChecksumInvalid(..))),
            "a SHA-256 that does not match"
        );
        assert!(!kept(&fixture));
        let (result, _) = try_with(PACKAGE.len() as u64 + 1, SHA256);
        assert!(
            matches!(result, Err(velopack::Error::SizeInvalid(..))),
            "a size that does not match"
        );
        assert!(!kept(&fixture));

        let (result, percents) = try_with(PACKAGE.len() as u64, SHA256);
        assert!(result.is_ok(), "{result:?}");
        assert!(kept(&fixture), "a matching package is kept for the install");
        assert_eq!(percents.last(), Some(&100));
    }
}
