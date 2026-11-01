// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/presentation.hpp"
#include <memory>

namespace metro::ui {
// Construct only for normal interactive runs, never --check or --stop.
// Owns no logger, timer, thread, global hotkey or game resource.
class WindowsConsole {
public:
    explicit WindowsConsole(bool allow_dashboard = true) noexcept;
    ~WindowsConsole() noexcept;
    WindowsConsole(const WindowsConsole&) = delete;
    WindowsConsole& operator=(const WindowsConsole&) = delete;
    // Call on model changes; exceptions and console failures never escape.
    void update(const Model& model) noexcept;
    // Call before polling hotkeys in the existing loop; checks navigation, redraws
    // only when needed. Sole consumer of console input records; bounded reads
    // of already queued events. No wait or logger writes. Left/Right page only
    // within the console; the runtime keeps all global feature hotkeys.
    void refresh() noexcept;
    // Outside the game, require matching key-down evidence from this refresh's
    // console input batch. Never grants persistent eligibility to a host HWND.
    bool hotkey_pressed(unsigned binding, bool async_rising, bool game_foreground) const noexcept;
    void navigate(Navigation direction) noexcept;
    bool dashboard_active() const noexcept;
    // Persistent diagnostic, nullptr if healthy. Caller may log it once.
    const char* error() const noexcept;
private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};
}
