//! Command-execution seam. Adapters that spawn external programs (`btrfs`,
//! `lsblk`, …) depend on [`CommandRunner`] rather than `std::process` directly,
//! so they stay unit-testable: production wires [`SystemCommandRunner`] (an argv
//! array, **never** a shell); tests inject a fake.

use std::ffi::OsStr;
use std::process::Command;

use mybtrfs_application::ports::PortError;

/// Runs an external program and returns its captured stdout.
pub(crate) trait CommandRunner {
    /// Run `program` with `args` (an argv array, no shell), returning stdout.
    ///
    /// # Errors
    /// [`PortError::Io`] if the process cannot be spawned; [`PortError::Command`]
    /// if it exits unsuccessfully (with its stderr in the message). A specific
    /// runner may surface other variants — see [`SystemCommandRunner`].
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError>;
}

/// Production [`CommandRunner`] over [`std::process::Command`].
///
/// Decodes stdout as strict UTF-8 (else [`PortError::Parse`]). btrfs output is
/// normally ASCII; a subvolume path containing non-UTF-8 bytes is therefore
/// unsupported — rejected, never lossily decoded (which would corrupt path
/// identity). Revisit with `OsString`-based parsing if that limitation bites.
pub(crate) struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&OsStr]) -> Result<String, PortError> {
        let output = Command::new(program).args(args).output()?;
        if !output.status.success() {
            return Err(PortError::Command(format!(
                "`{program}` exited unsuccessfully ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|err| {
            PortError::Parse(format!("`{program}` produced non-UTF-8 output: {err}"))
        })
    }
}
