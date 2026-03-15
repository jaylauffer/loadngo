#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};

#[cfg(windows)]
pub trait HostedComponent: ui_core::Component {
    fn hwnd(&self) -> HWND;
    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
}
