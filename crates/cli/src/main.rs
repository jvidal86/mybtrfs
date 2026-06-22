//! mybtrfs binary entry point — a thin shell over the `cli` module, which is the
//! composition root (wires concrete adapters into the use cases and dispatches).
//! See `documentation/02-architecture-v2.md`.

mod cli;

fn main() -> std::process::ExitCode {
    cli::run()
}
