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

use mybtrfs_application::ports::PortError;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn osargs(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
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
}
