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

// Run on macOS without Accessibility permission or GUI interaction:
// clang++ -std=c++11 espanso-mac-utils/tests/focus_native.mm \
//   -framework Cocoa -framework Carbon -framework ApplicationServices \
//   -o /tmp/espanso-focus-tests && /tmp/espanso-focus-tests
#include "../src/focus.mm"
#include <cassert>
#include <limits>
#include <unistd.h>

static void check_range(CFIndex location, CFIndex length, CFStringRef text, bool expected) {
  CFRange range = CFRangeMake(location, length);
  AXValueRef value = AXValueCreate(kAXValueTypeCFRange, &range);
  assert(value);
  assert(valid_selection(value, text) == expected);
  CFRelease(value);
}

int main() {
  @autoreleasepool {
    // AX ranges use UTF-16 offsets, including surrogate pairs. Allow an empty
    // selection at the end, but reject negative/out-of-bounds/overflow ranges.
    CFStringRef text = CFSTR("A😀B");
    assert(CFStringGetLength(text) == 4);
    check_range(1, 2, text, true);
    check_range(4, 0, text, true);
    check_range(0, 4, text, true);
    check_range(0, 0, CFSTR(""), true);
    check_range(-1, 0, text, false);
    check_range(0, -1, text, false);
    check_range(5, 0, text, false);
    check_range(3, 2, text, false);
    check_range(1, std::numeric_limits<CFIndex>::max(), text, false);
    assert(!valid_selection(nullptr, text));
    assert(!valid_selection(kCFBooleanTrue, text));
    CGPoint point = CGPointMake(0, 0);
    AXValueRef wrong_type = AXValueCreate(kAXValueTypeCGPoint, &point);
    assert(!valid_selection(wrong_type, text));
    CFRelease(wrong_type);

    CFRange range = CFRangeMake(1, 2);
    AXValueRef first = AXValueCreate(kAXValueTypeCFRange, &range);
    AXValueRef second = AXValueCreate(kAXValueTypeCFRange, &range);
    assert(CFEqual(first, second));
    CFRange replacement;
    assert(replacement_range(first, text, CFSTR(""), replacement));
    assert(replacement.location == 1 && replacement.length == 2);
    assert(!replacement_range(first, text, CFSTR("A"), replacement));
    CFRange caret = CFRangeMake(4, 0);
    AXValueRef caret_value = AXValueCreate(kAXValueTypeCFRange, &caret);
    assert(replacement_range(caret_value, text, CFSTR("😀B"), replacement));
    assert(replacement.location == 1 && replacement.length == 3);
    assert(!replacement_range(caret_value, text, CFSTR("bad"), replacement));
    assert(!replacement_range(caret_value, text, CFSTR("too long"), replacement));
    assert(!replacement_range(nullptr, text, CFSTR("A"), replacement));
    CFRelease(caret_value);
    caret = CFRangeMake(3, 0);
    caret_value = AXValueCreate(kAXValueTypeCFRange, &caret);
    assert(replacement_range(caret_value, CFSTR("xe\u0301"), CFSTR("e\u0301"), replacement));
    assert(replacement.location == 1 && replacement.length == 2);
    // Precomposed and decomposed text are not the same literal trigger suffix.
    assert(!replacement_range(caret_value, CFSTR("xe\u0301"), CFSTR("é"), replacement));
    CFRelease(caret_value);
    range.location = 2;
    AXValueRef moved = AXValueCreate(kAXValueTypeCFRange, &range);
    assert(!CFEqual(first, moved));
    CFRelease(first);
    CFRelease(second);
    CFRelease(moved);

    // A retained PID alone isn't a process identity. Start time is available
    // even without AX permission and must be stable between reads.
    uint64_t seconds = 0, microseconds = 0, again_seconds = 0, again_microseconds = 0;
    assert(process_start(getpid(), seconds, microseconds));
    assert(seconds > 0);
    assert(process_start(getpid(), again_seconds, again_microseconds));
    assert(seconds == again_seconds && microseconds == again_microseconds);
    assert(!process_start(-1, seconds, microseconds));

    assert(!mac_utils_focus_is_current(nullptr));
    assert(!mac_utils_focus_is_unchanged_current(nullptr));
    assert(!mac_utils_focus_restore(nullptr));
    assert(!mac_utils_focus_app_name(nullptr));
    assert(!mac_utils_focus_paste(nullptr, nullptr, 0, 1, 0));
    mac_utils_focus_destroy(nullptr);
  }
}
