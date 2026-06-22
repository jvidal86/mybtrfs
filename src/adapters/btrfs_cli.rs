//! `BtrfsCliAdapter` — spawns `btrfs` directly (argv array, **never** a shell);
//! implements `SubvolumeRepository`, `SnapshotPort`, `TransferPort`, `DeletePort`.
//! Verification (readonly + received_uuid + plausible parent_uuid) and
//! garbled-receive cleanup are part of the transfer contract — exit codes are
//! never trusted alone. See `documentation/04-coding-guidelines.md` §5.
//
// TODO (Phase 1+): one central command runner; careful send|receive piping.
