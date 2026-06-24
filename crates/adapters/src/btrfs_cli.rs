//! `BtrfsCliAdapter` — spawns `btrfs` directly (argv array, **never** a shell);
//! implements `SubvolumeRepository`, `SnapshotPort`, `TransferPort`, `DeletePort`.
//! Verification (readonly + received_uuid + plausible parent_uuid) and
//! garbled-receive cleanup are part of the transfer contract — exit codes are
//! never trusted alone. See `documentation/04-coding-guidelines.md` §5.

pub(crate) mod parse;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use mybtrfs_application::ports::{
    DeleteCommit, DeletePort, DiffPort, PortError, ProgressPort, SnapshotPort, SubvolumeRepository,
    TransferPort,
};
use mybtrfs_domain::model::{Subvolume, Uuid};
use mybtrfs_domain::parent::ParentSelection;

use crate::command::{CommandRunner, SystemCommandRunner};
use crate::mounts::{self, MountTable, ProcMounts};
use crate::ssh::{SshCommandRunner, SshEndpoint, SshMountTable, SshSourceRunner};

/// External program name (spawned as an argv array, never via a shell).
const BTRFS: &str = "btrfs";

/// The `btrfs` CLI adapter implementing the subvolume / snapshot / transfer /
/// delete ports. It resolves each queried path's filesystem (mountpoint +
/// `fs_uuid`) from the mount table, so a single instance serves any directory —
/// source and target alike — stamping every parsed [`Subvolume`] with the
/// filesystem it actually lives on (which the btrfs output itself does not carry).
pub struct BtrfsCliAdapter {
    runner: Box<dyn CommandRunner>,
    mounts: Box<dyn MountTable>,
    /// Optional progress reporter for [`TransferPort::send_receive`]. When
    /// `Some`, a byte-counting bridge thread measures throughput and calls
    /// [`ProgressPort::report_bytes`] every ~250 ms. `None` uses a direct kernel
    /// pipe (zero userspace copy overhead) and no progress is reported.
    progress: Option<Arc<dyn ProgressPort>>,
}

impl BtrfsCliAdapter {
    /// Create an adapter that spawns the real `btrfs` binary and resolves each
    /// path's filesystem from `/proc/self/mounts`. No progress reporting by
    /// default; call [`with_progress`](Self::with_progress) to enable it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: Box::new(SystemCommandRunner),
            mounts: Box::new(ProcMounts),
            progress: None,
        }
    }

    /// Attach a [`ProgressPort`] for transfer byte counting. Returns `self` for
    /// builder-style chaining.
    #[must_use]
    pub fn with_progress(mut self, progress: Arc<dyn ProgressPort>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Create an adapter whose every btrfs operation runs on a remote host over
    /// SSH (`ssh … -- sudo btrfs …`), resolving paths from the remote host's
    /// `/proc/self/mounts`. A transfer's `btrfs send` stays local; only the
    /// receive is remote. Serves the repository / transfer / delete ports for a
    /// remote backup **target** (Phase 5 §2). See `documentation/08-phase5-design.md`.
    #[must_use]
    pub fn ssh_target(endpoint: SshEndpoint) -> Self {
        Self {
            runner: Box::new(SshCommandRunner::new(
                Box::new(SystemCommandRunner),
                endpoint.clone(),
            )),
            mounts: Box::new(SshMountTable::new(Box::new(SystemCommandRunner), endpoint)),
            progress: None,
        }
    }

    /// Create the **transfer** adapter for restoring *from* a remote source
    /// (Phase 5 §2): the `btrfs send` of the remote backup runs over SSH while the
    /// `btrfs receive` into local staging — and the verification of the received
    /// copy — run locally (resolved from the local `/proc/self/mounts`). The mirror
    /// of [`ssh_target`](Self::ssh_target).
    #[must_use]
    pub fn ssh_source(endpoint: SshEndpoint) -> Self {
        Self {
            runner: Box::new(SshSourceRunner::new(
                Box::new(SystemCommandRunner),
                endpoint,
            )),
            mounts: Box::new(ProcMounts),
            progress: None,
        }
    }

    /// Resolve the btrfs filesystem containing `path`: its mountpoint (from the
    /// mount table) and its `fs_uuid` (from `btrfs filesystem show`).
    ///
    /// # Errors
    /// [`PortError`] if the mount table cannot be read, no btrfs filesystem
    /// contains `path`, or `btrfs filesystem show` fails / cannot be parsed.
    fn resolve(&self, path: &Path) -> Result<(PathBuf, Uuid, PathBuf), PortError> {
        let entries = self.mounts.entries()?;
        let mount = mounts::containing_btrfs_mount(&entries, path)
            .ok_or_else(|| PortError::Command(format!("no btrfs filesystem contains {path:?}")))?;
        let mountpoint = mount.mountpoint.clone();
        let subvol = mount.subvol.clone();
        let output = self.runner.run(
            BTRFS,
            &[
                OsStr::new("filesystem"),
                OsStr::new("show"),
                mountpoint.as_os_str(),
            ],
        )?;
        let fs_uuid = parse::parse_filesystem_uuid(&output)?;
        Ok((mountpoint, fs_uuid, subvol))
    }
}

impl Default for BtrfsCliAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SubvolumeRepository for BtrfsCliAdapter {
    fn show(&self, path: &Path) -> Result<Subvolume, PortError> {
        // `show` is given the real path, so its mountpoint-relative `relative_path`
        // is already correct; the mount subvol is only needed to re-base `list`.
        let (mountpoint, fs_uuid, _subvol) = self.resolve(path)?;
        let output = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("show"),
                path.as_os_str(),
            ],
        )?;
        parse::parse_show(
            &output,
            relative_path(path, &mountpoint),
            &fs_uuid,
            &mountpoint,
        )
    }

    fn list(&self, filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
        let (mountpoint, fs_uuid, subvol) = self.resolve(filesystem)?;
        // Display flags match btrbk: -a (all) -c (cgen) -u (uuid) -q (parent_uuid)
        // -R (received_uuid). The read-only flag is only available via -r, so a
        // second call provides it and `parse_list` merges the two.
        let listing = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("list"),
                OsStr::new("-a"),
                OsStr::new("-c"),
                OsStr::new("-u"),
                OsStr::new("-q"),
                OsStr::new("-R"),
                filesystem.as_os_str(),
            ],
        )?;
        let readonly = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("list"),
                OsStr::new("-a"),
                OsStr::new("-r"),
                filesystem.as_os_str(),
            ],
        )?;
        // `list` paths are fs-root-relative; re-base each to the mountpoint (using
        // the mounted subvolume) so it agrees with `show` and `mountpoint.join(path)`
        // reconstructs the real on-disk path on any mount layout (invariant: paths
        // are mountpoint-relative everywhere).
        let mut subvolumes = parse::parse_list(&listing, &readonly, &fs_uuid, &mountpoint)?;
        for subvolume in &mut subvolumes {
            subvolume.path = parse::to_mountpoint_relative(&subvolume.path, &subvol);
        }
        Ok(subvolumes)
    }
}

impl SnapshotPort for BtrfsCliAdapter {
    fn create_readonly(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError> {
        self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("snapshot"),
                OsStr::new("-r"),
                source.as_os_str(),
                dest.as_os_str(),
            ],
        )?;
        let created = self.show(dest)?;
        if !created.readonly {
            return Err(PortError::Verification(format!(
                "snapshot of {source:?} at {dest:?} is not read-only"
            )));
        }
        Ok(created)
    }

    fn make_writable(&self, source: &Path, dest: &Path) -> Result<Subvolume, PortError> {
        // No `-r`: the only sanctioned route to a writable subvolume (restore);
        // never `btrfs property set ro=false`, which poisons received_uuid (invariant #7).
        self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("snapshot"),
                source.as_os_str(),
                dest.as_os_str(),
            ],
        )?;
        let created = self.show(dest)?;
        if created.readonly {
            return Err(PortError::Verification(format!(
                "writable snapshot of {source:?} at {dest:?} is unexpectedly read-only"
            )));
        }
        Ok(created)
    }
}

impl TransferPort for BtrfsCliAdapter {
    fn send_receive(
        &self,
        source: &Subvolume,
        selection: &ParentSelection,
        target_dir: &Path,
    ) -> Result<Subvolume, PortError> {
        // Build the absolute source/parent/clone paths (each Subvolume carries its
        // own mountpoint), then `btrfs send [-p parent] [-c clone…] <source>`.
        let source_path = source.mountpoint.join(&source.path);
        let parent_path = selection
            .parent
            .as_ref()
            .map(|parent| parent.mountpoint.join(&parent.path));
        let clone_paths: Vec<PathBuf> = selection
            .clone_sources
            .iter()
            .map(|clone| clone.mountpoint.join(&clone.path))
            .collect();

        let mut send_args: Vec<&OsStr> = vec![OsStr::new("send")];
        if let Some(parent_path) = &parent_path {
            send_args.push(OsStr::new("-p"));
            send_args.push(parent_path.as_os_str());
        }
        for clone_path in &clone_paths {
            send_args.push(OsStr::new("-c"));
            send_args.push(clone_path.as_os_str());
        }
        send_args.push(source_path.as_os_str());

        let receive_args = [OsStr::new("receive"), target_dir.as_os_str()];

        // Build the optional byte-counting callback. When progress is wired,
        // the bridge thread calls this every ~250 ms; when None, a direct
        // kernel pipe is used (zero userspace copy overhead).
        let on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>> =
            self.progress.as_ref().map(|p| {
                let p = Arc::clone(p);
                Arc::new(move |total, speed| p.report_bytes(total, speed))
                    as Arc<dyn Fn(u64, u64) + Send + Sync>
            });

        // Send/receive, then ALWAYS inspect the target: even on a pipe error a
        // partially-received (garbled) subvolume may be left behind and must be
        // cleaned up (invariant #2); a success is never trusted on exit code (#1).
        let transfer = self
            .runner
            .pipe((BTRFS, &send_args), (BTRFS, &receive_args), on_progress);

        let received_name = source.path.file_name().ok_or_else(|| {
            PortError::Verification(format!("source subvolume {:?} has no name", source.path))
        })?;
        let received_path = target_dir.join(received_name);

        match self.show(&received_path) {
            Ok(received) => {
                if received.is_garbled() {
                    // btrfs-progs leaves garbled subvolumes behind; delete by hand.
                    if let Err(e) = self.delete(&received_path, DeleteCommit::Each) {
                        log::warn!(
                            "failed to clean up garbled backup at {}: {}; you may need to delete it manually",
                            received_path.display(),
                            e
                        );
                    }
                }
                transfer?; // a pipe failure surfaces here (garbled already cleaned)
                verify_received(&received, selection.parent.is_none())?;
                Ok(received)
            }
            Err(show_error) => {
                // No inspectable target: report the pipe failure if any, else the
                // fact that the receive silently produced nothing.
                transfer?;
                Err(show_error)
            }
        }
    }
}

impl DeletePort for BtrfsCliAdapter {
    fn delete(&self, path: &Path, commit: DeleteCommit) -> Result<(), PortError> {
        let mut args: Vec<&OsStr> = vec![OsStr::new("subvolume"), OsStr::new("delete")];
        if matches!(commit, DeleteCommit::Each) {
            args.push(OsStr::new("--commit-each"));
        }
        args.push(path.as_os_str());
        self.runner.run(BTRFS, &args)?;
        Ok(())
    }
}

impl DiffPort for BtrfsCliAdapter {
    fn referenced_bytes(&self, path: &Path) -> Result<u64, PortError> {
        log::debug!("btrfs subvolume show {:?} (referenced bytes)", path);
        let output = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("show"),
                path.as_os_str(),
            ],
        )?;
        parse::parse_referenced_bytes(&output)
    }

    fn find_new_changed_bytes(&self, path: &Path, since_gen: u64) -> Result<u64, PortError> {
        let gen_str = since_gen.to_string();
        log::debug!(
            "btrfs subvolume find-new {:?} {} (changed bytes)",
            path,
            gen_str
        );
        let output = self.runner.run(
            BTRFS,
            &[
                OsStr::new("subvolume"),
                OsStr::new("find-new"),
                path.as_os_str(),
                OsStr::new(&gen_str),
            ],
        )?;
        parse::parse_find_new_changed_bytes(&output)
    }
}

/// `path` relative to `mountpoint` (as stamped onto a `Subvolume`); falls back to
/// the input path if it is not under the mountpoint.
fn relative_path(path: &Path, mountpoint: &Path) -> PathBuf {
    path.strip_prefix(mountpoint)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Plausibility checks on a received subvolume (mirrors btrbk's post-receive
/// checks): read-only, `received_uuid` set, and `parent_uuid` matching the
/// transfer mode — absent for a full send, present for an incremental one.
fn verify_received(received: &Subvolume, full: bool) -> Result<(), PortError> {
    let mut problems: Vec<&str> = Vec::new();
    if !received.readonly {
        problems.push("target is not read-only");
    }
    if received.received_uuid.is_none() {
        problems.push("received_uuid is not set");
    }
    if full && received.parent_uuid.is_some() {
        problems.push("parent_uuid is set on a full receive");
    }
    if !full && received.parent_uuid.is_none() {
        problems.push("parent_uuid is not set on an incremental receive");
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(PortError::Verification(format!(
            "send/receive verification failed: {}",
            problems.join("; ")
        )))
    }
}

#[cfg(test)]
impl BtrfsCliAdapter {
    /// Test constructor injecting a fake command runner and mount table.
    fn with_parts(runner: Box<dyn CommandRunner>, mounts: Box<dyn MountTable>) -> Self {
        Self {
            runner,
            mounts,
            progress: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::mounts::MountEntry;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn fs() -> Uuid {
        Uuid::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap()
    }

    fn mountpoint() -> PathBuf {
        PathBuf::from("/mnt/pool")
    }

    /// A minimal source/parent subvolume (only path + mountpoint matter for the
    /// commands the adapter builds).
    fn source_subvol(path: &str) -> Subvolume {
        Subvolume {
            id: 256,
            uuid: Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111"),
            parent_uuid: None,
            received_uuid: None,
            generation: 100,
            cgen: 100,
            readonly: true,
            path: PathBuf::from(path),
            fs_uuid: fs(),
            mountpoint: mountpoint(),
        }
    }

    const SHOW: &str = "\
@data
    UUID:               a1a1a1a1-1111-4111-8111-111111111111
    Parent UUID:        -
    Received UUID:      -
    Subvolume ID:       256
    Generation:         120
    Gen at creation:    95
    Flags:              -
";

    const SHOW_READONLY: &str = "\
backups/@data.20260622T1900
    UUID:               c3c3c3c3-3333-4333-8333-333333333333
    Parent UUID:        a1a1a1a1-1111-4111-8111-111111111111
    Received UUID:      -
    Subvolume ID:       260
    Generation:         130
    Gen at creation:    130
    Flags:              readonly
";

    // A received full backup: read-only, received_uuid set, no parent_uuid.
    const RECEIVED_FULL: &str = "\
@data.20260622T1900
    UUID:               d4d4d4d4-4444-4444-8444-444444444444
    Parent UUID:        -
    Received UUID:      a1a1a1a1-1111-4111-8111-111111111111
    Subvolume ID:       300
    Generation:         5
    Gen at creation:    5
    Flags:              readonly
";

    // A received incremental backup: like the full one, but with parent_uuid set.
    const RECEIVED_INCREMENTAL: &str = "\
@data.20260622T1900
    UUID:               d4d4d4d4-4444-4444-8444-444444444444
    Parent UUID:        e5e5e5e5-5555-4555-8555-555555555555
    Received UUID:      a1a1a1a1-1111-4111-8111-111111111111
    Subvolume ID:       300
    Generation:         5
    Gen at creation:    5
    Flags:              readonly
";

    // A garbled receive: writable with no received_uuid (btrfs leaves these behind).
    const RECEIVED_GARBLED: &str = "\
@data.20260622T1900
    UUID:               d4d4d4d4-4444-4444-8444-444444444444
    Parent UUID:        -
    Received UUID:      -
    Subvolume ID:       300
    Generation:         5
    Gen at creation:    5
    Flags:              -
";

    const LIST: &str = "\
ID 256 gen 120 cgen 95 top level 5 parent_uuid - received_uuid - uuid a1a1a1a1-1111-4111-8111-111111111111 path @data
ID 260 gen 130 cgen 130 top level 5 parent_uuid b2b2b2b2-2222-4222-8222-222222222222 received_uuid a1a1a1a1-1111-4111-8111-111111111111 uuid c3c3c3c3-3333-4333-8333-333333333333 path <FS_TREE>/backups/@data.20260622T1900
";

    const READONLY: &str =
        "ID 260 gen 130 top level 5 path <FS_TREE>/backups/@data.20260622T1900\n";

    /// `btrfs filesystem show` output carrying the pool's fs UUID (== `fs()`).
    const FS_SHOW: &str = "Label: 'pool'  uuid: ffffffff-ffff-4fff-8fff-ffffffffffff\n";

    /// A `MountTable` exposing the pool and the drive as btrfs mounts.
    struct FakeMounts;
    impl MountTable for FakeMounts {
        fn entries(&self) -> Result<Vec<MountEntry>, PortError> {
            Ok(vec![
                MountEntry {
                    mountpoint: PathBuf::from("/mnt/pool"),
                    fstype: "btrfs".to_owned(),
                    subvol: PathBuf::from("/"),
                },
                MountEntry {
                    mountpoint: PathBuf::from("/mnt/drive"),
                    fstype: "btrfs".to_owned(),
                    subvol: PathBuf::from("/"),
                },
            ])
        }
    }

    /// Fake runner: returns canned output keyed on the btrfs subcommand, records
    /// every invocation (so tests can assert flags), and can simulate failure.
    struct FakeBtrfs {
        show: String,
        list: String,
        readonly: String,
        fail: bool,
        calls: Rc<RefCell<Vec<Vec<String>>>>,
    }

    impl Default for FakeBtrfs {
        fn default() -> Self {
            Self {
                show: SHOW.to_owned(),
                list: LIST.to_owned(),
                readonly: READONLY.to_owned(),
                fail: false,
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl FakeBtrfs {
        fn record(&self, program: &str, args: &[&OsStr]) {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
            self.calls.borrow_mut().push(call);
        }
    }

    impl CommandRunner for FakeBtrfs {
        fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError> {
            self.record(program, args);
            if self.fail {
                return Err(PortError::Command("simulated btrfs failure".to_owned()));
            }
            let has = |needle: &str| args.iter().any(|arg| *arg == OsStr::new(needle));
            // Routing doubles as a flag assertion: wrong flags fall through to Err.
            if has("filesystem") && has("show") {
                Ok(FS_SHOW.to_owned())
            } else if has("show") {
                Ok(self.show.clone())
            } else if has("list") && has("-r") {
                Ok(self.readonly.clone())
            } else if has("list") && has("-c") && has("-u") && has("-q") && has("-R") {
                Ok(self.list.clone())
            } else if has("snapshot") || has("delete") {
                Ok(String::new()) // snapshot/delete print only a success line, no parsed output
            } else {
                Err(PortError::Command(format!(
                    "unexpected btrfs invocation: {args:?}"
                )))
            }
        }

        fn pipe(
            &self,
            producer: (&str, &[&OsStr]),
            consumer: (&str, &[&OsStr]),
            _on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
        ) -> Result<(), PortError> {
            self.record(producer.0, producer.1);
            self.record(consumer.0, consumer.1);
            if self.fail {
                return Err(PortError::Command("simulated pipe failure".to_owned()));
            }
            Ok(())
        }
    }

    fn repo(runner: FakeBtrfs) -> BtrfsCliAdapter {
        BtrfsCliAdapter::with_parts(Box::new(runner), Box::new(FakeMounts))
    }

    /// Build an adapter over a fake whose recorded calls the test can inspect.
    fn recording_repo(fake: FakeBtrfs) -> (BtrfsCliAdapter, Rc<RefCell<Vec<Vec<String>>>>) {
        let calls = Rc::clone(&fake.calls);
        (
            BtrfsCliAdapter::with_parts(Box::new(fake), Box::new(FakeMounts)),
            calls,
        )
    }

    #[test]
    fn show_resolves_filesystem_and_tags_the_subvolume() {
        crate::init_test_logger();
        let sv = repo(FakeBtrfs::default())
            .show(Path::new("/mnt/pool/@data"))
            .unwrap();
        assert_eq!(sv.id, 256);
        assert_eq!(sv.uuid, Uuid::parse("a1a1a1a1-1111-4111-8111-111111111111"));
        assert!(!sv.readonly);
        assert_eq!(sv.path, PathBuf::from("@data")); // mountpoint stripped from the queried path
        assert_eq!(sv.fs_uuid, fs()); // resolved from `btrfs filesystem show`
        assert_eq!(sv.mountpoint, mountpoint()); // resolved from the mount table
    }

    #[test]
    fn list_merges_readonly_from_second_call() {
        crate::init_test_logger();
        let subs = repo(FakeBtrfs::default())
            .list(Path::new("/mnt/pool"))
            .unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].id, 256);
        assert!(!subs[0].readonly);
        assert_eq!(subs[1].id, 260);
        assert!(subs[1].readonly);
        assert_eq!(subs[1].fs_uuid, fs());
    }

    #[test]
    fn list_rebases_paths_for_a_non_root_subvol_mount() {
        crate::init_test_logger();
        // A pool mounted at a non-top-level subvolume (`subvol=/@pool`): btrfs
        // `list` reports fs-root-relative paths (under `@pool/`), which the adapter
        // must re-base to mountpoint-relative so `mountpoint.join(path)` is correct.
        struct SubvolMounts;
        impl MountTable for SubvolMounts {
            fn entries(&self) -> Result<Vec<MountEntry>, PortError> {
                Ok(vec![MountEntry {
                    mountpoint: PathBuf::from("/mnt/pool"),
                    fstype: "btrfs".to_owned(),
                    subvol: PathBuf::from("/@pool"),
                }])
            }
        }
        let list = "\
ID 256 gen 120 cgen 95 top level 256 parent_uuid - received_uuid - uuid a1a1a1a1-1111-4111-8111-111111111111 path <FS_TREE>/@pool/@data
ID 260 gen 130 cgen 130 top level 256 parent_uuid b2b2b2b2-2222-4222-8222-222222222222 received_uuid a1a1a1a1-1111-4111-8111-111111111111 uuid c3c3c3c3-3333-4333-8333-333333333333 path <FS_TREE>/@pool/backups/@data.20260622T1900
";
        let readonly =
            "ID 260 gen 130 top level 256 path <FS_TREE>/@pool/backups/@data.20260622T1900\n";
        let adapter = BtrfsCliAdapter::with_parts(
            Box::new(FakeBtrfs {
                list: list.to_owned(),
                readonly: readonly.to_owned(),
                ..FakeBtrfs::default()
            }),
            Box::new(SubvolMounts),
        );
        let subs = adapter.list(Path::new("/mnt/pool")).unwrap();
        assert_eq!(subs.len(), 2);
        // The `@pool/` mount-subvol prefix is stripped → mountpoint-relative,
        // matching what `show` would stamp.
        assert_eq!(subs[0].path, PathBuf::from("@data"));
        assert_eq!(subs[1].path, PathBuf::from("backups/@data.20260622T1900"));
        // So `mountpoint.join(path)` reconstructs the real on-disk path:
        assert_eq!(
            subs[1].mountpoint.join(&subs[1].path),
            PathBuf::from("/mnt/pool/backups/@data.20260622T1900")
        );
    }

    #[test]
    fn command_failure_propagates() {
        crate::init_test_logger();
        let err = repo(FakeBtrfs {
            fail: true,
            ..FakeBtrfs::default()
        })
        .show(Path::new("/mnt/pool/@data"))
        .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }

    #[test]
    fn create_readonly_returns_the_readonly_snapshot() {
        crate::init_test_logger();
        let sv = repo(FakeBtrfs {
            show: SHOW_READONLY.to_owned(),
            ..FakeBtrfs::default()
        })
        .create_readonly(
            Path::new("/mnt/pool/@data"),
            Path::new("/mnt/pool/backups/@data.20260622T1900"),
        )
        .unwrap();
        assert!(sv.readonly);
        assert_eq!(sv.id, 260);
    }

    #[test]
    fn create_readonly_rejects_a_writable_result() {
        crate::init_test_logger();
        // SHOW (the default) is writable, so the post-snapshot check must fail.
        let err = repo(FakeBtrfs::default())
            .create_readonly(
                Path::new("/mnt/pool/@data"),
                Path::new("/mnt/pool/backups/@data.20260622T1900"),
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Verification(_)));
    }

    #[test]
    fn make_writable_returns_a_writable_snapshot() {
        crate::init_test_logger();
        let sv = repo(FakeBtrfs::default()) // SHOW is writable
            .make_writable(
                Path::new("/mnt/pool/backups/@data.20260622T1900"),
                Path::new("/mnt/pool/restore/@data"),
            )
            .unwrap();
        assert!(!sv.readonly);
    }

    #[test]
    fn send_receive_full_backup_verifies_and_returns() {
        crate::init_test_logger();
        let received = repo(FakeBtrfs {
            show: RECEIVED_FULL.to_owned(),
            ..FakeBtrfs::default()
        })
        .send_receive(
            &source_subvol("snapshots/@data.20260622T1900"),
            &ParentSelection::default(),
            Path::new("/mnt/drive/host"),
        )
        .unwrap();
        assert!(received.readonly);
        assert!(received.received_uuid.is_some());
        assert_eq!(received.parent_uuid, None);
    }

    #[test]
    fn send_receive_incremental_accepts_set_parent_uuid() {
        crate::init_test_logger();
        let selection = ParentSelection {
            parent: Some(source_subvol("snapshots/@data.20260621T1900")),
            clone_sources: Vec::new(),
        };
        let received = repo(FakeBtrfs {
            show: RECEIVED_INCREMENTAL.to_owned(),
            ..FakeBtrfs::default()
        })
        .send_receive(
            &source_subvol("snapshots/@data.20260622T1900"),
            &selection,
            Path::new("/mnt/drive/host"),
        )
        .unwrap();
        assert!(received.parent_uuid.is_some());
    }

    #[test]
    fn send_receive_full_rejects_a_set_parent_uuid() {
        crate::init_test_logger();
        // RECEIVED_INCREMENTAL has parent_uuid set, but this is a full send.
        let err = repo(FakeBtrfs {
            show: RECEIVED_INCREMENTAL.to_owned(),
            ..FakeBtrfs::default()
        })
        .send_receive(
            &source_subvol("snapshots/@data.20260622T1900"),
            &ParentSelection::default(),
            Path::new("/mnt/drive/host"),
        )
        .unwrap_err();
        assert!(matches!(err, PortError::Verification(_)));
    }

    #[test]
    fn send_receive_deletes_a_garbled_result() {
        crate::init_test_logger();
        let (adapter, calls) = recording_repo(FakeBtrfs {
            show: RECEIVED_GARBLED.to_owned(),
            ..FakeBtrfs::default()
        });
        let err = adapter
            .send_receive(
                &source_subvol("snapshots/@data.20260622T1900"),
                &ParentSelection::default(),
                Path::new("/mnt/drive/host"),
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Verification(_)));
        // The garbled subvolume was deleted by hand.
        assert!(
            calls
                .borrow()
                .iter()
                .any(|call| call.iter().any(|arg| arg.as_str() == "delete"))
        );
    }

    #[test]
    fn send_receive_passes_parent_flag_for_incremental() {
        crate::init_test_logger();
        let (adapter, calls) = recording_repo(FakeBtrfs {
            show: RECEIVED_INCREMENTAL.to_owned(),
            ..FakeBtrfs::default()
        });
        let selection = ParentSelection {
            parent: Some(source_subvol("snapshots/@data.20260621T1900")),
            clone_sources: Vec::new(),
        };
        adapter
            .send_receive(
                &source_subvol("snapshots/@data.20260622T1900"),
                &selection,
                Path::new("/mnt/drive/host"),
            )
            .unwrap();
        // The send leg carried `-p <parent>`.
        assert!(
            calls
                .borrow()
                .iter()
                .any(|call| call.iter().any(|a| a.as_str() == "send")
                    && call.iter().any(|a| a.as_str() == "-p"))
        );
    }

    #[test]
    fn send_receive_propagates_pipe_failure() {
        crate::init_test_logger();
        let err = repo(FakeBtrfs {
            fail: true,
            ..FakeBtrfs::default()
        })
        .send_receive(
            &source_subvol("snapshots/@data.20260622T1900"),
            &ParentSelection::default(),
            Path::new("/mnt/drive/host"),
        )
        .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }

    #[test]
    fn delete_passes_commit_each_only_when_requested() {
        crate::init_test_logger();
        let (adapter, calls) = recording_repo(FakeBtrfs::default());
        adapter
            .delete(Path::new("/mnt/pool/snap"), DeleteCommit::Deferred)
            .unwrap();
        adapter
            .delete(Path::new("/mnt/pool/snap"), DeleteCommit::Each)
            .unwrap();

        let recorded = calls.borrow();
        assert_eq!(recorded.len(), 2);
        assert!(!recorded[0].iter().any(|a| a.as_str() == "--commit-each"));
        assert!(recorded[1].iter().any(|a| a.as_str() == "--commit-each"));
    }

    #[test]
    fn delete_failure_propagates() {
        crate::init_test_logger();
        let err = repo(FakeBtrfs {
            fail: true,
            ..FakeBtrfs::default()
        })
        .delete(Path::new("/mnt/pool/snap"), DeleteCommit::Deferred)
        .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }
}
