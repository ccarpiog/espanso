/*
 * This file is part of espanso.
 *
 * Copyright (C) 2026 Espanso contributors
 *
 * espanso is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * espanso is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with espanso.  If not, see <https://www.gnu.org/licenses/>.
 */

//! A best-effort macOS destination guard for delayed text insertion.
//!
//! Accessibility identity can confirm an app, window and text field, but cannot
//! make a later keyboard event atomic with that check. Call `is_current` again
//! immediately before dispatch, and retain a copy of the generated text.

use std::{ffi::CStr, marker::PhantomData, ptr::NonNull, rc::Rc};

use crate::ffi;

/// An owned snapshot of the frontmost application's focused text field.
///
/// Capture fails closed when Accessibility cannot identify a window and a
/// supported text input. This deliberately prefers manual paste over guessing a
/// destination (for example, a browser exposing only its entire web area).
/// The original text and selection are held in memory. Restoration refuses to
/// move the caret or insert if the original field's text changed while the form
/// was open. Unsupported text models and secure text fields fail capture.
///
/// The native references are released on drop. The guard stays on its creating
/// thread; no `Send` or `Sync` promise is made for retained Cocoa/AX objects.
pub struct FocusTarget {
    raw: NonNull<std::ffi::c_void>,
    _thread_bound: PhantomData<Rc<()>>,
}

impl FocusTarget {
    /// Capture the current app, window and editable text control, if available.
    pub fn capture() -> Option<Self> {
        // SAFETY: The native function returns null or a uniquely owned object.
        NonNull::new(unsafe { ffi::mac_utils_focus_capture() }).map(|raw| Self {
            raw,
            _thread_bound: PhantomData,
        })
    }

    /// Attempt to restore the original window and field, then verify all three
    /// identities. Each AX request has a short timeout, and polling is bounded.
    /// An unavailable/closed target returns false, never an app-only success.
    pub fn restore_and_verify(&self) -> bool {
        // SAFETY: `raw` stays owned and valid until Drop; native code does not
        // retain the pointer after this call.
        unsafe { ffi::mac_utils_focus_restore(self.raw.as_ptr()) != 0 }
    }

    /// Confirm app, window, field, text and selection immediately before the
    /// first deletion/insertion. Unlike `is_current`, this check must not be
    /// used after dispatch has intentionally changed the destination text.
    pub fn is_unchanged_current(&self) -> bool {
        // SAFETY: see restore_and_verify.
        unsafe { ffi::mac_utils_focus_is_unchanged_current(self.raw.as_ptr()) != 0 }
    }

    /// Confirm app, window and field are still current without changing focus.
    /// This deliberately ignores text/selection changes made during dispatch.
    pub fn is_current(&self) -> bool {
        // SAFETY: see restore_and_verify.
        unsafe { ffi::mac_utils_focus_is_current(self.raw.as_ptr()) != 0 }
    }

    /// Submit a guarded select-trigger / Cmd+V / cursor-left sequence to the
    /// captured process, without deferring it onto the main dispatch queue.
    ///
    /// The clipboard must already contain the final expansion. The original
    /// text and selection are verified before the first event; focus identity
    /// is checked before each key press. The exact expected trigger suffix must
    /// precede the captured caret; it is selected using UTF-16 AX offsets, then
    /// replaced by paste. If expected_deletion is empty, the original selection
    /// is preserved. Keys are always released to their original PID, even if
    /// focus changes. A false return may follow a partial sequence: keep a copy
    /// for recovery and never retry automatically. A true return
    /// means events were submitted, not that the application accepted a paste.
    pub fn paste_from_clipboard(
        &self,
        expected_deletion: &str,
        cursor_moves: usize,
        delay_ms: u32,
    ) -> bool {
        // SAFETY: raw is owned, the UTF-8 slice stays borrowed throughout this call, and native event
        // ownership and release stay within the synchronous call.
        unsafe {
            ffi::mac_utils_focus_paste(
                self.raw.as_ptr(),
                expected_deletion.as_ptr(),
                expected_deletion.len(),
                cursor_moves,
                delay_ms,
            ) != 0
        }
    }

    /// Human-readable application name, captured with the destination.
    pub fn app_name(&self) -> String {
        // SAFETY: Native code returns a NUL-terminated string owned by this
        // snapshot and valid until Drop. Copy it before releasing the borrow.
        let name = unsafe { ffi::mac_utils_focus_app_name(self.raw.as_ptr()) };
        if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for FocusTarget {
    fn drop(&mut self) {
        // SAFETY: raw came from capture, has exactly one owner, and this is its
        // only destruction. No other safe API exposes the pointer.
        unsafe { ffi::mac_utils_focus_destroy(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_focus_api_rejects_null_handles() {
        // Exercise failure semantics without querying or changing GUI focus.
        unsafe {
            assert_eq!(ffi::mac_utils_focus_is_current(std::ptr::null()), 0);
            assert_eq!(ffi::mac_utils_focus_restore(std::ptr::null()), 0);
            assert_eq!(
                ffi::mac_utils_focus_is_unchanged_current(std::ptr::null()),
                0
            );
            assert_eq!(
                ffi::mac_utils_focus_paste(std::ptr::null(), std::ptr::null(), 0, 1, 0),
                0
            );
            assert!(ffi::mac_utils_focus_app_name(std::ptr::null()).is_null());
            ffi::mac_utils_focus_destroy(std::ptr::null_mut());
        }
    }
}
