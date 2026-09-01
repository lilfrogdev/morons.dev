#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{NativeError, OperationPaths, OperationProfile};
