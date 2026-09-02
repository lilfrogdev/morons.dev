#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    CommandCompletion, CommandLaunch, CommandLimits, CommandProcess, NativeError, OperationPaths,
    OperationProfile,
};
