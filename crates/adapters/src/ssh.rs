//! SSH endpoint addressing + command construction for Phase 5 §2 (remote
//! source/target). A remote btrfs operation is the **same** btrfs argv the local
//! adapter builds, executed as `ssh [opts] [user@]host -- [sudo] btrfs <argv>`.
//!
//! Crucially, `ssh` itself runs **locally** and connects out, so the existing
//! [`CommandRunner`](crate::command::CommandRunner) executes it unchanged —
//! including `btrfs send … | ssh host -- sudo btrfs receive …` through `pipe`.
//! The SSH-awareness is therefore *only* argv construction (no spawning), so this
//! module is pure and fully unit-testable. See `documentation/08-phase5-design.md`
//! §2.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;

use mybtrfs_application::ports::PortError;

use crate::command::CommandRunner;
use crate::mounts::{MountEntry, MountTable, parse_mounts};

/// The scheme that marks a remote endpoint.
const SSH_SCHEME: &str = "ssh://";

/// A remote host reachable over SSH, addressed as `[user@]host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshEndpoint {
    /// Login user (`user@`); `None` lets ssh use its own default.
    pub user: Option<String>,
    /// Hostname or IP.
    pub host: String,
    /// TCP port (`-p`); `None` uses ssh's default (22 / `ssh_config`).
    pub port: Option<u16>,
}

impl SshEndpoint {
    /// Build the local `ssh` invocation that runs `program args…` on the remote,
    /// optionally via `sudo` (btrfs needs root remotely).
    ///
    /// Non-interactive (`BatchMode=yes` — never block on a password) and
    /// shell-free: the remote argv is passed verbatim after `--`, so the
    /// no-shell-interpolation rule holds over SSH exactly as it does locally.
    #[must_use]
    pub fn command(
        &self,
        sudo: bool,
        program: &str,
        args: &[&OsStr],
    ) -> (&'static str, Vec<OsString>) {
        let mut argv: Vec<OsString> = vec![OsString::from("-o"), OsString::from("BatchMode=yes")];
        if let Some(port) = self.port {
            argv.push(OsString::from("-p"));
            argv.push(OsString::from(port.to_string()));
        }
        argv.push(OsString::from(self.destination()));
        argv.push(OsString::from("--"));
        if sudo {
            argv.push(OsString::from("sudo"));
        }
        argv.push(OsString::from(program));
        for arg in args {
            argv.push(arg.to_os_string());
        }
        ("ssh", argv)
    }

    /// `[user@]host`.
    #[must_use]
    fn destination(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

/// A backup endpoint: a path on the local machine, or a path on a remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A path on the local filesystem.
    Local(PathBuf),
    /// A path on a remote host reached over SSH.
    Remote {
        /// The SSH host.
        ssh: SshEndpoint,
        /// The absolute path on the remote host.
        path: PathBuf,
    },
}

/// Parse a backup source/target spec: `ssh://[user@]host[:port]/abs/path` is a
/// remote endpoint; anything else is a local path (so existing CLI usage is
/// unchanged).
///
/// # Errors
/// [`PortError::Parse`] if an `ssh://` spec lacks a host, lacks a path, or has a
/// non-numeric port.
pub fn parse_endpoint(spec: &str) -> Result<Endpoint, PortError> {
    let Some(rest) = spec.strip_prefix(SSH_SCHEME) else {
        return Ok(Endpoint::Local(PathBuf::from(spec)));
    };
    // rest = [user@]host[:port]/abs/path — split at the FIRST '/' (the path keeps
    // its leading slash, so it is absolute on the remote).
    let slash = rest
        .find('/')
        .ok_or_else(|| PortError::Parse(format!("ssh endpoint has no path: {spec}")))?;
    let (authority, path) = rest.split_at(slash);

    let (user, hostport) = match authority.split_once('@') {
        Some((user, hostport)) => (Some(user.to_owned()), hostport),
        None => (None, authority),
    };
    let (host, port) = match hostport.split_once(':') {
        Some((host, port_text)) => {
            let port: u16 = port_text
                .parse()
                .map_err(|_| PortError::Parse(format!("ssh endpoint has a bad port: {spec}")))?;
            (host.to_owned(), Some(port))
        }
        None => (hostport.to_owned(), None),
    };
    if host.is_empty() {
        return Err(PortError::Parse(format!(
            "ssh endpoint has no host: {spec}"
        )));
    }
    Ok(Endpoint::Remote {
        ssh: SshEndpoint { user, host, port },
        path: PathBuf::from(path),
    })
}

/// Build the `&[&OsStr]` borrow of an owned `argv` for [`CommandRunner`].
fn arg_refs(argv: &[OsString]) -> Vec<&OsStr> {
    argv.iter().map(OsString::as_os_str).collect()
}

/// A [`CommandRunner`] that runs each btrfs command on a remote host over SSH
/// (`ssh host -- sudo btrfs …`) by delegating to a **local** runner — the `ssh`
/// process is itself local, so no new execution machinery is needed. For a
/// transfer it keeps the producer (`btrfs send`) local and wraps only the consumer
/// (`btrfs receive`) in ssh, since a backup's source is local and only its target
/// is remote. This lets the whole `BtrfsCliAdapter` serve a remote target unchanged.
pub(crate) struct SshCommandRunner {
    inner: Box<dyn CommandRunner>,
    endpoint: SshEndpoint,
}

impl SshCommandRunner {
    /// Wrap `inner` (a local runner) so btrfs commands run on `endpoint`.
    pub(crate) fn new(inner: Box<dyn CommandRunner>, endpoint: SshEndpoint) -> Self {
        Self { inner, endpoint }
    }
}

impl CommandRunner for SshCommandRunner {
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError> {
        let (ssh_program, ssh_args) = self.endpoint.command(true, program, args);
        self.inner.run(ssh_program, &arg_refs(&ssh_args))
    }

    fn pipe(
        &self,
        producer: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), PortError> {
        // The producer (`btrfs send`) stays local; only the consumer (`btrfs
        // receive`) runs on the remote target.
        let (consumer_program, consumer_args) = consumer;
        let (ssh_program, ssh_args) = self.endpoint.command(true, consumer_program, consumer_args);
        self.inner
            .pipe(producer, (ssh_program, &arg_refs(&ssh_args)), on_progress)
    }
}

/// A [`CommandRunner`] for the **reverse** transfer direction — restoring *from* a
/// remote source. Single commands (the verify `show` of the locally-received copy)
/// run **locally**; for the `send | receive` pipe it wraps only the **producer**
/// (`btrfs send` of the remote backup) in `ssh host -- sudo …`, leaving the
/// consumer (`btrfs receive` into local staging) local. The mirror image of
/// [`SshCommandRunner`], which wraps the consumer for backup *to* a remote target.
pub(crate) struct SshSourceRunner {
    inner: Box<dyn CommandRunner>,
    endpoint: SshEndpoint,
}

impl SshSourceRunner {
    /// Wrap `inner` (a local runner) so the *send* side runs on `endpoint`.
    pub(crate) fn new(inner: Box<dyn CommandRunner>, endpoint: SshEndpoint) -> Self {
        Self { inner, endpoint }
    }
}

impl CommandRunner for SshSourceRunner {
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError> {
        // Local: verifies the locally-received copy / resolves its filesystem.
        self.inner.run(program, args)
    }

    fn pipe(
        &self,
        producer: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), PortError> {
        // The producer (`btrfs send` of the remote backup) runs over ssh; the
        // consumer (`btrfs receive` into local staging) stays local.
        let (producer_program, producer_args) = producer;
        let (ssh_program, ssh_args) = self.endpoint.command(true, producer_program, producer_args);
        self.inner
            .pipe((ssh_program, &arg_refs(&ssh_args)), consumer, on_progress)
    }
}

/// A [`MountTable`] that reads the **remote** host's `/proc/self/mounts` over SSH
/// (no sudo — it is world-readable), so `BtrfsCliAdapter` resolves a remote-target
/// path to its filesystem exactly as it does locally.
pub(crate) struct SshMountTable {
    inner: Box<dyn CommandRunner>,
    endpoint: SshEndpoint,
}

impl SshMountTable {
    /// Wrap `inner` (a local runner) so the mount table is read from `endpoint`.
    pub(crate) fn new(inner: Box<dyn CommandRunner>, endpoint: SshEndpoint) -> Self {
        Self { inner, endpoint }
    }
}

impl MountTable for SshMountTable {
    fn entries(&self) -> Result<Vec<MountEntry>, PortError> {
        let (ssh_program, ssh_args) =
            self.endpoint
                .command(false, "cat", &[OsStr::new("/proc/self/mounts")]);
        let content = self.inner.run(ssh_program, &arg_refs(&ssh_args))?;
        parse_mounts(&content)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn osargs(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// A shared log of `(program, args)` each call was asked to run.
    type CallLog = Rc<RefCell<Vec<(String, Vec<String>)>>>;

    /// Records each call into a shared [`CallLog`], so a wrapping runner's argv
    /// transformation can be asserted.
    struct RecordingRunner {
        log: CallLog,
        stdout: String,
    }
    fn strs(args: &[&OsStr]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError> {
            self.log.borrow_mut().push((program.to_owned(), strs(args)));
            Ok(self.stdout.clone())
        }
        fn pipe(
            &self,
            producer: (&str, &[&OsStr]),
            consumer: (&str, &[&OsStr]),
            _on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
        ) -> Result<(), PortError> {
            self.log
                .borrow_mut()
                .push((format!("PIPE {}|{}", producer.0, consumer.0), {
                    let mut v = strs(producer.1);
                    v.push("|".to_owned());
                    v.extend(strs(consumer.1));
                    v
                }));
            Ok(())
        }
    }

    #[test]
    fn a_plain_path_is_a_local_endpoint() {
        crate::init_test_logger();
        assert_eq!(
            parse_endpoint("/mnt/backup/host").unwrap(),
            Endpoint::Local(PathBuf::from("/mnt/backup/host"))
        );
    }

    #[test]
    fn parses_a_full_ssh_endpoint() {
        crate::init_test_logger();
        assert_eq!(
            parse_endpoint("ssh://isard@apolo:2222/mnt/btrfs-test").unwrap(),
            Endpoint::Remote {
                ssh: SshEndpoint {
                    user: Some("isard".to_owned()),
                    host: "apolo".to_owned(),
                    port: Some(2222),
                },
                path: PathBuf::from("/mnt/btrfs-test"),
            }
        );
    }

    #[test]
    fn parses_a_minimal_ssh_endpoint() {
        crate::init_test_logger();
        assert_eq!(
            parse_endpoint("ssh://apolo/mnt/backup").unwrap(),
            Endpoint::Remote {
                ssh: SshEndpoint {
                    user: None,
                    host: "apolo".to_owned(),
                    port: None,
                },
                path: PathBuf::from("/mnt/backup"),
            }
        );
    }

    #[test]
    fn rejects_an_ssh_endpoint_without_a_path_host_or_port() {
        crate::init_test_logger();
        assert!(parse_endpoint("ssh://apolo").is_err()); // no path
        assert!(parse_endpoint("ssh:///mnt/backup").is_err()); // no host
        assert!(parse_endpoint("ssh://apolo:notaport/mnt").is_err()); // bad port
    }

    #[test]
    fn builds_a_sudo_remote_receive_invocation() {
        crate::init_test_logger();
        let ep = SshEndpoint {
            user: Some("isard".to_owned()),
            host: "apolo".to_owned(),
            port: None,
        };
        let (program, args) = ep.command(
            true,
            "btrfs",
            &[OsStr::new("receive"), OsStr::new("/mnt/btrfs-test")],
        );
        assert_eq!(program, "ssh");
        assert_eq!(
            args,
            osargs(&[
                "-o",
                "BatchMode=yes",
                "isard@apolo",
                "--",
                "sudo",
                "btrfs",
                "receive",
                "/mnt/btrfs-test",
            ])
        );
    }

    #[test]
    fn includes_the_port_and_omits_sudo_when_not_requested() {
        crate::init_test_logger();
        let ep = SshEndpoint {
            user: None,
            host: "apolo".to_owned(),
            port: Some(2222),
        };
        let (_, args) = ep.command(
            false,
            "btrfs",
            &[OsStr::new("subvolume"), OsStr::new("show")],
        );
        assert_eq!(
            args,
            osargs(&[
                "-o",
                "BatchMode=yes",
                "-p",
                "2222",
                "apolo",
                "--",
                "btrfs",
                "subvolume",
                "show",
            ])
        );
    }

    /// `Vec<String>` form of an expected recorded argv.
    fn ss(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    fn apolo() -> SshEndpoint {
        SshEndpoint {
            user: None,
            host: "apolo".to_owned(),
            port: None,
        }
    }

    #[test]
    fn ssh_runner_wraps_each_btrfs_command_in_ssh_sudo() {
        crate::init_test_logger();
        let log = Rc::new(RefCell::new(Vec::new()));
        let runner = SshCommandRunner::new(
            Box::new(RecordingRunner {
                log: Rc::clone(&log),
                stdout: String::new(),
            }),
            apolo(),
        );
        runner
            .run(
                "btrfs",
                &[
                    OsStr::new("subvolume"),
                    OsStr::new("delete"),
                    OsStr::new("/mnt/btrfs-test/x"),
                ],
            )
            .unwrap();
        assert_eq!(
            log.borrow()[0],
            (
                "ssh".to_owned(),
                ss(&[
                    "-o",
                    "BatchMode=yes",
                    "apolo",
                    "--",
                    "sudo",
                    "btrfs",
                    "subvolume",
                    "delete",
                    "/mnt/btrfs-test/x",
                ])
            )
        );
    }

    #[test]
    fn ssh_runner_keeps_the_send_local_and_only_wraps_the_receive() {
        crate::init_test_logger();
        let log = Rc::new(RefCell::new(Vec::new()));
        let runner = SshCommandRunner::new(
            Box::new(RecordingRunner {
                log: Rc::clone(&log),
                stdout: String::new(),
            }),
            SshEndpoint {
                user: Some("isard".to_owned()),
                host: "apolo".to_owned(),
                port: None,
            },
        );
        runner
            .pipe(
                ("btrfs", &[OsStr::new("send"), OsStr::new("/pool/snap")]),
                (
                    "btrfs",
                    &[OsStr::new("receive"), OsStr::new("/mnt/btrfs-test")],
                ),
                None,
            )
            .unwrap();
        // Producer is still a local `btrfs send`; only the consumer is ssh-wrapped.
        assert_eq!(
            log.borrow()[0],
            (
                "PIPE btrfs|ssh".to_owned(),
                ss(&[
                    "send",
                    "/pool/snap",
                    "|",
                    "-o",
                    "BatchMode=yes",
                    "isard@apolo",
                    "--",
                    "sudo",
                    "btrfs",
                    "receive",
                    "/mnt/btrfs-test",
                ])
            )
        );
    }

    #[test]
    fn ssh_source_runner_wraps_the_send_in_ssh_and_keeps_the_receive_local() {
        crate::init_test_logger();
        let log = Rc::new(RefCell::new(Vec::new()));
        let runner = SshSourceRunner::new(
            Box::new(RecordingRunner {
                log: Rc::clone(&log),
                stdout: String::new(),
            }),
            SshEndpoint {
                user: Some("isard".to_owned()),
                host: "apolo".to_owned(),
                port: None,
            },
        );
        runner
            .pipe(
                (
                    "btrfs",
                    &[OsStr::new("send"), OsStr::new("/mnt/btrfs-test/data.X")],
                ),
                (
                    "btrfs",
                    &[OsStr::new("receive"), OsStr::new("/pool/staging")],
                ),
                None,
            )
            .unwrap();
        // Producer (send) is ssh-wrapped; consumer (receive) stays a local btrfs.
        assert_eq!(
            log.borrow()[0],
            (
                "PIPE ssh|btrfs".to_owned(),
                ss(&[
                    "-o",
                    "BatchMode=yes",
                    "isard@apolo",
                    "--",
                    "sudo",
                    "btrfs",
                    "send",
                    "/mnt/btrfs-test/data.X",
                    "|",
                    "receive",
                    "/pool/staging",
                ])
            )
        );
    }

    #[test]
    fn ssh_source_runner_runs_single_commands_locally() {
        crate::init_test_logger();
        let log = Rc::new(RefCell::new(Vec::new()));
        let runner = SshSourceRunner::new(
            Box::new(RecordingRunner {
                log: Rc::clone(&log),
                stdout: String::new(),
            }),
            apolo(),
        );
        // The verify `show` of the locally-received copy is NOT ssh-wrapped.
        runner
            .run(
                "btrfs",
                &[
                    OsStr::new("subvolume"),
                    OsStr::new("show"),
                    OsStr::new("/pool/staging/data.X"),
                ],
            )
            .unwrap();
        assert_eq!(
            log.borrow()[0],
            (
                "btrfs".to_owned(),
                ss(&["subvolume", "show", "/pool/staging/data.X"])
            )
        );
    }

    #[test]
    fn ssh_mount_table_reads_the_remote_proc_self_mounts() {
        crate::init_test_logger();
        let log = Rc::new(RefCell::new(Vec::new()));
        let table = SshMountTable::new(
            Box::new(RecordingRunner {
                log: Rc::clone(&log),
                stdout: "/dev/loop0 /mnt/btrfs-test btrfs rw,relatime 0 0\n".to_owned(),
            }),
            apolo(),
        );
        let entries = table.entries().unwrap();
        // Ran `ssh apolo -- cat /proc/self/mounts` (no sudo — world-readable) ...
        assert_eq!(
            log.borrow()[0],
            (
                "ssh".to_owned(),
                ss(&[
                    "-o",
                    "BatchMode=yes",
                    "apolo",
                    "--",
                    "cat",
                    "/proc/self/mounts",
                ])
            )
        );
        // ... and parsed the remote btrfs mount.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mountpoint, PathBuf::from("/mnt/btrfs-test"));
        assert_eq!(entries[0].fstype, "btrfs");
    }
}
