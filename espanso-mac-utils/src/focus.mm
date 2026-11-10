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

#include "native.h"

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#include <libproc.h>

#include <chrono>
#include <memory>
#include <limits>
#include <new>
#include <string>
#include <thread>

namespace {
// A stalled application must not keep the form worker waiting indefinitely.
constexpr float AX_TIMEOUT_SECONDS = 0.1f;

class OwnedAX {
  AXUIElementRef value;

public:
  explicit OwnedAX(AXUIElementRef value = nullptr) : value(value) {
    if (value && AXUIElementSetMessagingTimeout(value, AX_TIMEOUT_SECONDS) != kAXErrorSuccess) {
      CFRelease(value);
      this->value = nullptr;
    }
  }
  ~OwnedAX() { if (value) CFRelease(value); }
  OwnedAX(const OwnedAX &) = delete;
  OwnedAX &operator=(const OwnedAX &) = delete;
  AXUIElementRef get() const { return value; }
  AXUIElementRef release() { auto result = value; value = nullptr; return result; }
};

// The synchronous Process Manager query avoids both NSWorkspace's run-loop
// cache and changing the process-wide AX timeout on a system-wide AX object.
pid_t frontmost_pid() {
  ProcessSerialNumber process;
  pid_t pid = -1;
  if (GetFrontProcess(&process) != noErr || GetProcessPID(&process, &pid) != noErr) return -1;
  return pid;
}

bool process_start(pid_t pid, uint64_t &seconds, uint64_t &microseconds) {
  proc_bsdinfo info = {};
  if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, sizeof(info)) != sizeof(info)) return false;
  seconds = info.pbi_start_tvsec;
  microseconds = info.pbi_start_tvusec;
  return true;
}

AXUIElementRef copy_element(AXUIElementRef source, CFStringRef attribute) {
  if (!source) return nullptr;
  CFTypeRef result = nullptr;
  if (AXUIElementCopyAttributeValue(source, attribute, &result) != kAXErrorSuccess) {
    if (result) CFRelease(result);
    return nullptr;
  }
  if (!result) return nullptr;
  if (CFGetTypeID(result) != AXUIElementGetTypeID()) {
    CFRelease(result);
    return nullptr;
  }
  return (AXUIElementRef)result;
}

CFTypeRef copy_attribute(AXUIElementRef element, CFStringRef attribute, CFTypeID expected_type) {
  CFTypeRef value = nullptr;
  if (AXUIElementCopyAttributeValue(element, attribute, &value) != kAXErrorSuccess) {
    if (value) CFRelease(value);
    return nullptr;
  }
  if (value && CFGetTypeID(value) != expected_type) {
    CFRelease(value);
    return nullptr;
  }
  return value;
}

bool valid_selection(CFTypeRef selection, CFStringRef text) {
  if (!selection || !text || CFGetTypeID(selection) != AXValueGetTypeID() ||
      AXValueGetType((AXValueRef)selection) != kAXValueTypeCFRange) return false;
  CFRange range;
  if (!AXValueGetValue((AXValueRef)selection, kAXValueTypeCFRange, &range)) return false;
  const auto length = CFStringGetLength(text);
  // Subtraction avoids overflow from an untrusted range returned by an app.
  return range.location >= 0 && range.length >= 0 && range.location <= length &&
      range.length <= length - range.location;
}

bool is_text_input(AXUIElementRef element) {
  CFTypeRef role = nullptr;
  if (AXUIElementCopyAttributeValue(element, kAXRoleAttribute, &role) != kAXErrorSuccess) {
    if (role) CFRelease(role);
    return false;
  }
  if (!role) return false;
  const bool supported = CFGetTypeID(role) == CFStringGetTypeID() &&
      (CFEqual(role, kAXTextFieldRole) || CFEqual(role, kAXTextAreaRole) ||
       CFEqual(role, kAXComboBoxRole));
  CFRelease(role);
  CFTypeRef subrole = copy_attribute(element, kAXSubroleAttribute, CFStringGetTypeID());
  const bool secure = subrole && CFEqual(subrole, kAXSecureTextFieldSubrole);
  if (subrole) CFRelease(subrole);
  return supported && !secure;
}

struct FocusSnapshot {
  NSRunningApplication *running_app;
  pid_t pid;
  uint64_t start_seconds = 0;
  uint64_t start_microseconds = 0;
  OwnedAX app;
  OwnedAX window;
  OwnedAX field;
  std::string name;
  CFStringRef text;
  AXValueRef selection;

  FocusSnapshot(NSRunningApplication *running_app, pid_t pid, AXUIElementRef app,
                AXUIElementRef window, AXUIElementRef field, CFStringRef text, AXValueRef selection)
      : running_app([running_app retain]), pid(pid), app(app), window(window), field(field),
        text(text), selection(selection) {
    const char *utf8_name = [[running_app localizedName] UTF8String];
    if (utf8_name) name = utf8_name;
    process_start(pid, start_seconds, start_microseconds);
  }
  ~FocusSnapshot() {
    [running_app release];
    if (text) CFRelease(text);
    if (selection) CFRelease(selection);
  }

  bool text_unchanged() const {
    CFTypeRef current = copy_attribute(field.get(), kAXValueAttribute, CFStringGetTypeID());
    const bool same = current && CFEqual(text, current);
    if (current) CFRelease(current);
    return same;
  }

  bool selection_equals(AXValueRef expected) const {
    CFTypeRef current = copy_attribute(field.get(), kAXSelectedTextRangeAttribute, AXValueGetTypeID());
    const bool same = valid_selection(current, text) && CFEqual(expected, current);
    if (current) CFRelease(current);
    return same;
  }

  bool selection_unchanged() const { return selection_equals(selection); }

  bool is_unchanged_current() const {
    return is_current() && text_unchanged() && selection_unchanged() && is_current();
  }

  bool is_current() const {
    if (!app.get() || !window.get() || !field.get() || [running_app isTerminated] ||
        frontmost_pid() != pid) return false;
    uint64_t current_seconds = 0, current_microseconds = 0;
    if (!start_seconds || !process_start(pid, current_seconds, current_microseconds) ||
        start_seconds != current_seconds || start_microseconds != current_microseconds) return false;
    OwnedAX current_window(copy_element(app.get(), kAXFocusedWindowAttribute));
    OwnedAX current_field(copy_element(app.get(), kAXFocusedUIElementAttribute));
    if (!current_window.get() || !current_field.get() ||
        !CFEqual(window.get(), current_window.get()) ||
        !CFEqual(field.get(), current_field.get())) return false;
    // Re-read the frontmost app after remote queries; app/window/field queries
    // are not an atomic OS operation. A final dispatch check is still needed.
    return frontmost_pid() == pid;
  }
};
// Preallocate the release event before sending a press. Releasing a key is
// unconditional, even when the target loses focus between the two events.
class TargetKey {
  pid_t pid;
  CGEventRef down;
  CGEventRef up;
  bool pressed = false;

public:
  TargetKey(pid_t pid, CGKeyCode key, CGEventFlags down_flags, CGEventFlags up_flags)
      : pid(pid), down(CGEventCreateKeyboardEvent(nullptr, key, true)),
        up(CGEventCreateKeyboardEvent(nullptr, key, false)) {
    if (down) {
      CGEventSetFlags(down, down_flags);
      CGEventSetLocation(down, CGPointMake(-27469, 0));
    }
    if (up) {
      CGEventSetFlags(up, up_flags);
      CGEventSetLocation(up, CGPointMake(-27469, 0));
    }
  }
  ~TargetKey() {
    release();
    if (down) CFRelease(down);
    if (up) CFRelease(up);
  }
  TargetKey(const TargetKey &) = delete;
  TargetKey &operator=(const TargetKey &) = delete;
  bool valid() const { return down && up; }
  bool press(const FocusSnapshot *target, int64_t clipboard_count = -1) {
    if (!valid() || !target->is_current()) return false;
    if (clipboard_count >= 0 && [[NSPasteboard generalPasteboard] changeCount] != clipboard_count)
      return false;
    CGEventPostToPid(pid, down);
    pressed = true;
    return true;
  }
  void release() {
    if (pressed) {
      CGEventPostToPid(pid, up);
      pressed = false;
    }
  }
};

bool replacement_range(AXValueRef selection, CFStringRef text,
                       CFStringRef deletion, CFRange &range) {
  if (!valid_selection(selection, text) || !deletion ||
      !AXValueGetValue(selection, kAXValueTypeCFRange, &range)) return false;
  const auto length = CFStringGetLength(deletion);
  if (!length) return true; // Preserve ordinary selected-text paste semantics.
  if (range.length != 0 || length > range.location) return false;
  range = CFRangeMake(range.location - length, length);
  return CFStringCompareWithOptions(text, deletion, range, 0) == kCFCompareEqualTo;
}

int64_t verified_clipboard_count(NSPasteboard *pasteboard, NSString *expected_text) {
  if (!pasteboard || !expected_text) return -1;
  const auto before = [pasteboard changeCount];
  NSString *actual_text = [pasteboard stringForType:NSPasteboardTypeString];
  const bool matches = actual_text && [actual_text isEqualToString:expected_text];
  const auto after = [pasteboard changeCount];
  return matches && before >= 0 && before == after ? after : -1;
}

} // namespace

int64_t mac_utils_clipboard_snapshot(const uint8_t *expected_text, size_t text_size) {
  @autoreleasepool {
    if (!expected_text && text_size) return -1;
    NSString *text = [[NSString alloc] initWithBytes:expected_text length:text_size encoding:NSUTF8StringEncoding];
    const auto count = verified_clipboard_count([NSPasteboard generalPasteboard], text);
    [text release];
    return count;
  }
}

void *mac_utils_focus_capture() {
  @autoreleasepool {
    if (!AXIsProcessTrusted()) return nullptr;
    const pid_t pid = frontmost_pid();
    if (pid <= 0) return nullptr;
    OwnedAX app(AXUIElementCreateApplication(pid));
    if (!app.get()) return nullptr;
    NSRunningApplication *running_app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    if (!running_app || [running_app isTerminated]) return nullptr;
    OwnedAX window(copy_element(app.get(), kAXFocusedWindowAttribute));
    OwnedAX field(copy_element(app.get(), kAXFocusedUIElementAttribute));
    if (!window.get() || !field.get() || !is_text_input(field.get())) return nullptr;
    // Reject a detached control or an inconsistent snapshot across windows.
    OwnedAX field_window(copy_element(field.get(), kAXWindowAttribute));
    if (!field_window.get() || !CFEqual(window.get(), field_window.get())) return nullptr;
    CFTypeRef text = copy_attribute(field.get(), kAXValueAttribute, CFStringGetTypeID());
    CFTypeRef selection = copy_attribute(field.get(), kAXSelectedTextRangeAttribute, AXValueGetTypeID());
    // Very large, unknown or invalid text models are safer to paste manually.
    if (!text || CFStringGetLength((CFStringRef)text) > 1024 * 1024 ||
        !valid_selection(selection, (CFStringRef)text)) {
      if (text) CFRelease(text);
      if (selection) CFRelease(selection);
      return nullptr;
    }
    std::unique_ptr<FocusSnapshot> target(new (std::nothrow) FocusSnapshot(
        running_app, pid, app.get(), window.get(), field.get(),
        (CFStringRef)text, (AXValueRef)selection));
    if (!target) {
      CFRelease(text);
      CFRelease(selection);
      return nullptr;
    }
    app.release();
    window.release();
    field.release();
    if (!target->is_unchanged_current()) return nullptr;
    return target.release();
  }
}

void mac_utils_focus_destroy(void *target) {
  @autoreleasepool { delete static_cast<FocusSnapshot *>(target); }
}

int32_t mac_utils_focus_is_current(const void *target) {
  @autoreleasepool {
    return target && static_cast<const FocusSnapshot *>(target)->is_current();
  }
}

int32_t mac_utils_focus_is_unchanged_current(const void *target) {
  @autoreleasepool {
    return target && static_cast<const FocusSnapshot *>(target)->is_unchanged_current();
  }
}

int32_t mac_utils_focus_restore(const void *raw_target) {
  @autoreleasepool {
    if (!raw_target) return 0;
    const auto *target = static_cast<const FocusSnapshot *>(raw_target);
    if ([target->running_app isTerminated]) return 0;
    if (target->is_unchanged_current()) return 1;
    if (!target->text_unchanged()) return 0;
    // NSRunningApplication is thread-safe. No dispatch_sync to the main queue:
    // the caller may already be there, or it may be waiting for this worker.
    if (![target->running_app activateWithOptions:NSApplicationActivateIgnoringOtherApps]) return 0;
    // Some apps don't support setting AXMain/AXFocused. Treat these only as
    // requests: success is exclusively determined by identity verification.
    AXUIElementSetAttributeValue(target->window.get(), kAXMainAttribute, kCFBooleanTrue);
    AXUIElementPerformAction(target->window.get(), kAXRaiseAction);
    AXUIElementSetAttributeValue(target->field.get(), kAXFocusedAttribute, kCFBooleanTrue);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(500);
    do {
      if (target->is_current()) {
        // Never move the selection in text that changed while the form was open.
        if (!target->text_unchanged()) return 0;
        if (!target->selection_unchanged() &&
            AXUIElementSetAttributeValue(target->field.get(), kAXSelectedTextRangeAttribute,
                                         target->selection) != kAXErrorSuccess) return 0;
        return target->is_unchanged_current();
      }
      std::this_thread::sleep_for(std::chrono::milliseconds(25));
    } while (std::chrono::steady_clock::now() < deadline);
    return 0;
  }
}

const char *mac_utils_focus_app_name(const void *target) {
  return target ? static_cast<const FocusSnapshot *>(target)->name.c_str() : nullptr;
}

int32_t mac_utils_focus_paste(const void *raw_target, const uint8_t *expected_deletion,
                              size_t deletion_size, size_t cursor_moves, uint32_t delay_ms,
                              int64_t clipboard_count) {
  @autoreleasepool {
    if (!raw_target || clipboard_count < 0 || (!expected_deletion && deletion_size) ||
        deletion_size > static_cast<size_t>(std::numeric_limits<CFIndex>::max())) return 0;
    const auto *target = static_cast<const FocusSnapshot *>(raw_target);
    CFStringRef deletion = CFStringCreateWithBytes(kCFAllocatorDefault, expected_deletion,
        static_cast<CFIndex>(deletion_size), kCFStringEncodingUTF8, false);
    if (!deletion) return 0;
    CFRange range;
    const bool valid = replacement_range(target->selection, target->text, deletion, range);
    CFRelease(deletion);
    if (!valid || !target->is_unchanged_current() ||
        [[NSPasteboard generalPasteboard] changeCount] != clipboard_count) return 0;
    AXValueRef replacement_selection = AXValueCreate(kAXValueTypeCFRange, &range);
    if (!replacement_selection) return 0;
    const auto delay = std::chrono::milliseconds(delay_ms);
    TargetKey command(target->pid, kVK_Command, kCGEventFlagMaskCommand, 0);
    TargetKey paste(target->pid, kVK_ANSI_V, kCGEventFlagMaskCommand, kCGEventFlagMaskCommand);
    TargetKey left(target->pid, kVK_LeftArrow, 0, 0);
    // Select the exact, verified trigger suffix instead of pressing backspace.
    // If paste is rejected, the original text is intact, merely selected.
    bool prepared = command.valid() && paste.valid() && left.valid();
    if (prepared && !CFEqual(target->selection, replacement_selection)) {
      prepared = AXUIElementSetAttributeValue(target->field.get(), kAXSelectedTextRangeAttribute,
                                              replacement_selection) == kAXErrorSuccess;
    }
    auto ready_to_paste = [&]() {
      return target->is_current() && target->text_unchanged() &&
          target->selection_equals(replacement_selection) && target->is_current() &&
          [[NSPasteboard generalPasteboard] changeCount] == clipboard_count;
    };
    if (!prepared || !ready_to_paste() || !command.press(target, clipboard_count)) {
      CFRelease(replacement_selection);
      return 0;
    }
    std::this_thread::sleep_for(delay);
    if (!ready_to_paste() || !paste.press(target, clipboard_count)) {
      CFRelease(replacement_selection);
      return 0; // TargetKey releases Command even on this failure path.
    }
    CFRelease(replacement_selection);
    paste.release();
    std::this_thread::sleep_for(delay);
    command.release();
    std::this_thread::sleep_for(delay);
    for (size_t i = 0; i < cursor_moves; ++i) {
      if (!left.press(target)) return 0;
      left.release();
      std::this_thread::sleep_for(delay);
    }
    return target->is_current();
  }
}
