//! `LocalFsAdapter` (`std::fs`) — implements `FilesystemPort`: path existence
//! checks, directory creation (default snapshot/target dirs), and rename /
//! move-aside (`D → D.broken` on restore). These are plain filesystem ops, not
//! btrfs commands — kept separate from `BtrfsCliAdapter` (SRP).
//
// TODO (Phase 1 / Phase 4).
