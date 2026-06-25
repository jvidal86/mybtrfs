//! Command-execution seam. Adapters that spawn external programs (`btrfs`,
//! `lsblk`, …) depend on [`CommandRunner`] rather than `std::process` directly,
//! so they stay unit-testable: production wires [`SystemCommandRunner`] (an argv
//! array, **never** a shell); tests inject a fake.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

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
    /// When `on_progress` is `Some`, a userspace bridge thread reads from the
    /// producer's stdout, counts bytes, calls the callback with `(total_bytes,
    /// bytes_per_sec)` every ~250 ms, and forwards data to the consumer. When
    /// `None`, the producer's stdout is connected directly to the consumer's
    /// stdin via a kernel pipe (zero userspace copy overhead).
    ///
    /// # Errors
    /// [`PortError::Io`] if either process cannot be spawned; [`PortError::Command`]
    /// if either exits unsuccessfully (with the relevant stderr in the message).
    fn pipe(
        &self,
        producer: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), PortError>;

    /// Run `producer | middle | consumer` as a three-stage pipeline for raw
    /// stream targets: `btrfs send | [zstd/gzip/xz] | [gpg/dd]`.
    ///
    /// Wait order: consumer → middle → producer (prevents deadlock).
    /// Exit-code check order: producer → middle → consumer (root-cause first).
    ///
    /// # Errors
    /// [`PortError::Io`] if any process cannot be spawned;
    /// [`PortError::Command`] if any exits unsuccessfully.
    ///
    /// # Panics
    /// The default implementation panics — existing [`CommandRunner`] implementors
    /// compile without change; only [`SystemCommandRunner`] overrides this.
    fn pipe3(
        &self,
        producer: (&str, &[&OsStr]),
        middle: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), PortError> {
        let _ = (producer, middle, consumer, on_progress);
        panic!(
            "pipe3 is not implemented for this CommandRunner; \
             use SystemCommandRunner for raw stream transfers"
        )
    }
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
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
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
        let mut producer_stdout = producer_child.stdout.take().ok_or_else(|| {
            PortError::Command(format!("`{}` exposed no stdout pipe", producer.0))
        })?;
        let producer_stderr = producer_child.stderr.take();

        let consumer_stdin: Stdio = if let Some(cb) = on_progress {
            // Instrumented path: insert a counting bridge between producer
            // stdout and consumer stdin so we can measure throughput.
            let (pipe_reader, pipe_writer) = std::io::pipe()?;
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                let mut total: u64 = 0;
                let start = std::time::Instant::now();
                let mut last_report = std::time::Instant::now();
                let mut writer = pipe_writer;
                loop {
                    let n = match producer_stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    total += n as u64;
                    if last_report.elapsed() >= std::time::Duration::from_millis(250) {
                        let secs = start.elapsed().as_secs_f64();
                        let speed = if secs > 0.0 {
                            (total as f64 / secs) as u64
                        } else {
                            0
                        };
                        cb(total, speed);
                        last_report = std::time::Instant::now();
                    }
                }
                // Final report after transfer completes.
                let secs = start.elapsed().as_secs_f64();
                let speed = if secs > 0.0 {
                    (total as f64 / secs) as u64
                } else {
                    0
                };
                cb(total, speed);
                // `writer` is dropped here, sending EOF to consumer's stdin.
            });
            pipe_reader.into()
        } else {
            // Fast path: direct kernel pipe, zero userspace copy.
            producer_stdout.into()
        };

        let consumer_child = Command::new(consumer.0)
            .args(consumer.1)
            .stdin(consumer_stdin)
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

    fn pipe3(
        &self,
        producer: (&str, &[&OsStr]),
        middle: (&str, &[&OsStr]),
        consumer: (&str, &[&OsStr]),
        on_progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<(), PortError> {
        // Two kernel pipes: producer→middle (pipe1) and middle→consumer (pipe2).
        // After each spawn the write-end is consumed by the child — the parent
        // never holds an open write-end, so no extra `drop()` is needed.
        log::debug!("pipe3: {} | {} | {}", producer.0, middle.0, consumer.0);

        let (pipe1_read, pipe1_write) = std::io::pipe()?;
        let (pipe2_read, pipe2_write) = std::io::pipe()?;

        // Spawn producer: stdout → pipe1_write (moved into child).
        log::debug!("spawning producer (send): {} {:?}", producer.0, producer.1);
        let mut producer_child = Command::new(producer.0)
            .args(producer.1)
            .stdout(pipe1_write) // pipe1_write consumed here
            .stderr(Stdio::piped())
            .spawn()?;
        let producer_stderr_handle = producer_child.stderr.take();

        // Optional progress bridge between pipe1 and middle.
        let middle_stdin: Stdio = if let Some(cb) = on_progress {
            let (bridge_read, bridge_write) = std::io::pipe()?;
            std::thread::spawn(move || {
                let mut reader = pipe1_read;
                let mut writer = bridge_write;
                let mut buf = [0u8; 65536];
                let mut total: u64 = 0;
                let start = std::time::Instant::now();
                let mut last_report = std::time::Instant::now();
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    total += n as u64;
                    if last_report.elapsed() >= std::time::Duration::from_millis(250) {
                        let secs = start.elapsed().as_secs_f64();
                        let speed = if secs > 0.0 {
                            (total as f64 / secs) as u64
                        } else {
                            0
                        };
                        cb(total, speed);
                        last_report = std::time::Instant::now();
                    }
                }
                let secs = start.elapsed().as_secs_f64();
                let speed = if secs > 0.0 {
                    (total as f64 / secs) as u64
                } else {
                    0
                };
                cb(total, speed);
            });
            bridge_read.into()
        } else {
            pipe1_read.into()
        };

        // Spawn middle: stdin ← pipe1 (or bridge), stdout → pipe2_write (moved into child).
        log::debug!("spawning middle (compress): {} {:?}", middle.0, middle.1);
        let mut middle_child = Command::new(middle.0)
            .args(middle.1)
            .stdin(middle_stdin)
            .stdout(pipe2_write) // pipe2_write consumed here
            .stderr(Stdio::piped())
            .spawn()?;
        let middle_stderr_handle = middle_child.stderr.take();

        // Spawn consumer: stdin ← pipe2_read (moved into child); writes output
        // via its own argv (e.g. `gpg -o <file>` or `dd of=<file>`).
        log::debug!(
            "spawning consumer (encrypt/write): {} {:?}",
            consumer.0,
            consumer.1
        );
        let consumer_child = Command::new(consumer.0)
            .args(consumer.1)
            .stdin(pipe2_read) // pipe2_read consumed here
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Drain producer and middle stderr on threads to prevent them from
        // blocking the whole pipeline by filling an undrained pipe buffer.
        let producer_stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut h) = producer_stderr_handle {
                let _ = h.read_to_end(&mut buf);
            }
            buf
        });
        let middle_stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut h) = middle_stderr_handle {
                let _ = h.read_to_end(&mut buf);
            }
            buf
        });

        // Wait consumer-first to avoid deadlock: if we waited producer first
        // while it is blocked writing to a full pipe1, the whole pipeline stalls.
        let consumer_out = consumer_child.wait_with_output()?;
        let middle_status = middle_child.wait()?;
        let producer_status = producer_child.wait()?;
        let producer_stderr = producer_stderr_thread.join().unwrap_or_default();
        let middle_stderr = middle_stderr_thread.join().unwrap_or_default();

        // Report root cause first: producer failure (broken pipe propagates
        // downstream, so the downstream exits are consequences, not causes).
        if !producer_status.success() {
            return Err(PortError::Command(format!(
                "`{}` (send) failed ({producer_status}): {}",
                producer.0,
                String::from_utf8_lossy(&producer_stderr).trim()
            )));
        }
        if !middle_status.success() {
            return Err(PortError::Command(format!(
                "`{}` (compress) failed ({middle_status}): {}",
                middle.0,
                String::from_utf8_lossy(&middle_stderr).trim()
            )));
        }
        if !consumer_out.status.success() {
            return Err(PortError::Command(format!(
                "`{}` (encrypt/write) failed ({}): {}",
                consumer.0,
                consumer_out.status,
                String::from_utf8_lossy(&consumer_out.stderr).trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_captures_stdout_of_successful_command() {
        let runner = SystemCommandRunner;
        let out = runner.run("echo", &[OsStr::new("hello")]).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn run_returns_command_error_on_nonzero_exit() {
        let runner = SystemCommandRunner;
        let err = runner
            .run("sh", &[OsStr::new("-c"), OsStr::new("exit 42")])
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
        assert!(err.to_string().contains("sh"));
    }

    #[test]
    fn pipe_direct_connects_producer_to_consumer() {
        // echo "hello" | cat  — consumer output is discarded, success is enough.
        let runner = SystemCommandRunner;
        runner
            .pipe(("echo", &[OsStr::new("hello")]), ("cat", &[]), None)
            .unwrap();
    }

    #[test]
    fn pipe_instrumented_invokes_progress_callback() {
        use std::sync::{Arc, Mutex};
        let runner = SystemCommandRunner;
        let reported = Arc::new(Mutex::new(Vec::<u64>::new()));
        let reported_clone = Arc::clone(&reported);

        runner
            .pipe(
                ("echo", &[OsStr::new("hello from pipe")]),
                ("cat", &[]),
                Some(Arc::new(move |total, _speed| {
                    reported_clone.lock().unwrap().push(total);
                })),
            )
            .unwrap();

        // The bridge thread emits at least one final progress report.
        let calls = reported.lock().unwrap();
        assert!(
            !calls.is_empty(),
            "progress callback should be called at least once"
        );
        assert!(*calls.last().unwrap() > 0, "total bytes should be > 0");
    }

    #[test]
    fn pipe_returns_error_when_producer_fails() {
        let runner = SystemCommandRunner;
        let err = runner
            .pipe(
                ("sh", &[OsStr::new("-c"), OsStr::new("exit 1")]),
                ("cat", &[]),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
        assert!(err.to_string().contains("send"));
    }

    #[test]
    fn pipe_returns_error_when_consumer_fails() {
        let runner = SystemCommandRunner;
        let err = runner
            .pipe(
                ("echo", &[OsStr::new("data")]),
                ("sh", &[OsStr::new("-c"), OsStr::new("exit 2")]),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
        assert!(err.to_string().contains("receive"));
    }

    // ── pipe3 tests ──────────────────────────────────────────────────────────

    #[test]
    fn pipe3_passes_data_through_all_stages() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        // echo "hello pipe3" | cat | dd of=<file>
        let mut of_arg = std::ffi::OsString::from("of=");
        of_arg.push(out_file.as_os_str());
        let runner = SystemCommandRunner;
        runner
            .pipe3(
                ("echo", &[OsStr::new("hello pipe3")]),
                ("cat", &[]),
                ("dd", &[OsStr::new(&of_arg)]),
                None,
            )
            .unwrap();
        let content = std::fs::read_to_string(&out_file).unwrap();
        assert!(content.contains("hello pipe3"));
    }

    #[test]
    fn pipe3_returns_send_error_when_producer_fails() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        let mut of_arg = std::ffi::OsString::from("of=");
        of_arg.push(out_file.as_os_str());
        let runner = SystemCommandRunner;
        let err = runner
            .pipe3(
                ("sh", &[OsStr::new("-c"), OsStr::new("exit 7")]),
                ("cat", &[]),
                ("dd", &[OsStr::new(&of_arg)]),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
        assert!(
            err.to_string().contains("send"),
            "expected 'send' in: {err}"
        );
    }

    #[test]
    fn pipe3_returns_compress_error_when_middle_fails() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        let mut of_arg = std::ffi::OsString::from("of=");
        of_arg.push(out_file.as_os_str());
        let runner = SystemCommandRunner;
        // Producer succeeds, middle fails, consumer fails (broken pipe from middle).
        // Error should mention middle (compress) unless producer also exits non-zero.
        let err = runner
            .pipe3(
                ("echo", &[OsStr::new("data")]),
                ("sh", &[OsStr::new("-c"), OsStr::new("exit 5")]),
                ("dd", &[OsStr::new(&of_arg)]),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
        // Producer succeeds; the error should be from middle or consumer.
        assert!(
            err.to_string().contains("compress") || err.to_string().contains("encrypt"),
            "expected 'compress' or 'encrypt' in: {err}"
        );
    }

    #[test]
    fn pipe3_instrumented_invokes_progress_callback() {
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        let mut of_arg = std::ffi::OsString::from("of=");
        of_arg.push(out_file.as_os_str());
        let reported = Arc::new(Mutex::new(Vec::<u64>::new()));
        let reported_clone = Arc::clone(&reported);
        let runner = SystemCommandRunner;
        runner
            .pipe3(
                ("echo", &[OsStr::new("hello instrumented pipe3")]),
                ("cat", &[]),
                ("dd", &[OsStr::new(&of_arg)]),
                Some(Arc::new(move |total, _speed| {
                    reported_clone.lock().unwrap().push(total);
                })),
            )
            .unwrap();
        let calls = reported.lock().unwrap();
        assert!(!calls.is_empty(), "progress callback must be called");
        assert!(*calls.last().unwrap() > 0, "total bytes must be > 0");
    }
}
