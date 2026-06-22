//! Drive discovery — implements `DriveDiscoveryPort`. Enumerates mounted btrfs
//! filesystems from `/proc/mounts`, enriched with removable/transport/size hints
//! from `lsblk --json`. Read-only; never acts on a device. See `documentation/01`
//! Phase 1.
//
// TODO (Phase 1).
