//! Command-execution seam. Adapters that spawn external programs (`btrfs`,
//! `lsblk`, …) depend on [`CommandRunner`] rather than `std::process` directly,
//! so they stay unit-testable: production wires [`SystemCommandRunner`] (an argv
//! array, **never** a shell); tests inject a fake.

use std::ffi::OsStr;
use std::io::Read;
use std::process::{Command, Stdio};

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

    /// Run `producer | consumer`: spawn both, pipe the producer's stdout into the
    /// consumer's stdin, and wait on both. For `btrfs send … | btrfs receive …`.
    /// Each argument is `(program, args)`.
    ///
    /// # Errors
    /// [`PortError::Io`] if either process cannot be spawned; [`PortError::Command`]
    /// if either exits unsuccessfully (with the relevant stderr in the message).
    fn pipe(
        &self,
        producer: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
    ) -> Result<(), PortError>;
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
        log::debug!("running: {program} {args:?}");
        let output = Command::new(program).args(args).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::debug!("command failed ({}): {}", output.status, stderr.trim());
            return Err(PortError::Command(format!(
                "`{program}` exited unsuccessfully ({}): {}",
                output.status,
                stderr.trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|err| {
            PortError::Parse(format!("`{program}` produced non-UTF-8 output: {err}"))
        })
    }

    fn pipe(
        &self,
        producer: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
    ) -> Result<(), PortError> {
        // Spawn the producer with its stdout piped, then hand that pipe to the
        // consumer's stdin. Ordering matters (04 §8): the consumer drains the data
        // pipe, so we wait on it first; the producer's stderr is drained on a thread
        // so a chatty producer can never deadlock by filling an undrained pipe.
        log::debug!("piping: {} | {}", producer.0, consumer.0);
        let mut producer_child = Command::new(producer.0)
            .args(producer.1)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let producer_stdout = producer_child.stdout.take().ok_or_else(|| {
            PortError::Command(format!("`{}` exposed no stdout pipe", producer.0))
        })?;
        let producer_stderr = producer_child.stderr.take();

        let consumer_child = Command::new(consumer.0)
            .args(consumer.1)
            .stdin(Stdio::from(producer_stdout))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stderr_drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut stderr) = producer_stderr {
                let _ = stderr.read_to_end(&mut buf);
            }
            buf
        });

        let consumer_out = consumer_child.wait_with_output()?;
        let producer_status = producer_child.wait()?;
        let producer_stderr = stderr_drain.join().unwrap_or_default();

        if !producer_status.success() {
            return Err(PortError::Command(format!(
                "`{}` (send) failed ({producer_status}): {}",
                producer.0,
                String::from_utf8_lossy(&producer_stderr).trim()
            )));
        }
        if !consumer_out.status.success() {
            return Err(PortError::Command(format!(
                "`{}` (receive) failed ({}): {}",
                consumer.0,
                consumer_out.status,
                String::from_utf8_lossy(&consumer_out.stderr).trim()
            )));
        }
        Ok(())
    }
}
