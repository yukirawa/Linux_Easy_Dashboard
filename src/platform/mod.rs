//! Platform access. Linux only, on purpose: the target is Arch Linux and we
//! prefer reading `/proc` over pulling in a cross platform monitoring crate.

pub mod os;
pub mod procfs;
pub mod sysfs;
