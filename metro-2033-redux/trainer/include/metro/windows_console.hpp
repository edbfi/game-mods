// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/presentation.hpp"
#include <memory>

namespace metro::ui {
#ifdef METRO_CONSOLE_TESTING
// Deterministic provenance injection for disposable console tests only.
void test_classic_foreground(bool foreground) noexcept;
#endif
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
    // Classic foreground accepts delivered event taps. Otherwise match an async
    // edge with console evidence during the same physical hold.
    // Call once per binding each input poll, including releases. Presentation
    // updates do not consume input. Game foreground preserves async behavior.
    bool hotkey_pressed(unsigned binding, bool down, bool async_rising, bool game_foreground) noexcept;
    bool dashboard_active() const noexcept;
    // Persistent diagnostic, nullptr if healthy. Caller may log it once.
    const char* error() const noexcept;
private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};
}
