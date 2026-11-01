// SPDX-License-Identifier: AGPL-3.0-only
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include "metro/windows_console.hpp"
#include <windows.h>
#include <algorithm>
#include <optional>
#include <set>
#include <utility>

namespace metro::ui {
struct WindowsConsole::Impl {
    HWND window = GetConsoleWindow();
    LONG_PTR window_style = 0;
    bool style_saved = false;
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    HANDLE in = GetStdHandle(STD_INPUT_HANDLE);
    DWORD input_mode = 0, output_mode = 0;
    CONSOLE_CURSOR_INFO cursor{};
    CONSOLE_SCREEN_BUFFER_INFO saved{};
    bool input_saved = false, output_saved = false, cursor_saved = false, info_saved = false;
    bool allow = true, active = false, dirty = true, up = false, down = false, cursor_hidden = false;
    const char* failure = nullptr;
    std::optional<Model> model;
    Frame previous;
    std::string previous_plain;
    SMALL_RECT viewport{};
    std::size_t page = 0;
    std::set<unsigned> pressed;

    explicit Impl(bool enabled) : allow(enabled) {
        output_saved = GetConsoleMode(out,&output_mode) != 0;
        input_saved = GetConsoleMode(in,&input_mode) != 0;
        cursor_saved = GetConsoleCursorInfo(out,&cursor) != 0;
        info_saved = GetConsoleScreenBufferInfo(out,&saved) != 0;
        // Do not enable raw input: processed Ctrl+C and all other flags survive.
        if (allow && output_saved && input_saved && info_saved && cursor_saved) {
            if (!SetConsoleMode(in,(input_mode | ENABLE_EXTENDED_FLAGS) & ~ENABLE_QUICK_EDIT_MODE)) {
                failure = "Console setup failed; using plain output.";
                allow = false;
            }
            // Classic conhost opens its system menu for Alt+Space before key
            // records reach us. Remove that menu for our console, then restore
            // the exact style on exit. This cannot control a ConPTY host's menu
            // or shortcut interception: GetConsoleWindow is hidden there.
            window_style = window ? GetWindowLongPtrW(window,GWL_STYLE) : 0;
            if (window_style & WS_SYSMENU) {
                SetLastError(0);
                const auto old = SetWindowLongPtrW(window,GWL_STYLE,window_style & ~WS_SYSMENU);
                style_saved = old != 0 || GetLastError() == 0;
                if (!style_saved) failure = "Cannot disable console system menu; choose a master binding other than Alt+Space.";
            }
        } else allow = false;
    }
    ~Impl() noexcept {
        if (style_saved) SetWindowLongPtrW(window,GWL_STYLE,window_style);
        if (input_saved) SetConsoleMode(in,input_mode);
        if (output_saved) SetConsoleMode(out,output_mode);
        if (info_saved) {
            SetConsoleTextAttribute(out,saved.wAttributes);
            CONSOLE_SCREEN_BUFFER_INFO now{};
            if (GetConsoleScreenBufferInfo(out,&now)) {
                auto position = saved.dwCursorPosition;
                position.X = std::min(position.X,static_cast<SHORT>(now.dwSize.X-1));
                position.Y = std::min(position.Y,static_cast<SHORT>(now.dwSize.Y-1));
                SetConsoleCursorPosition(out,position);
            }
        }
        if (cursor_saved) SetConsoleCursorInfo(out,&cursor);
    }
    WORD attributes(Tone tone) const {
        switch (tone) {
        case Tone::Amber: return FOREGROUND_RED | FOREGROUND_GREEN | FOREGROUND_INTENSITY;
        case Tone::Muted: return FOREGROUND_RED | FOREGROUND_GREEN | FOREGROUND_BLUE;
        case Tone::Error: return FOREGROUND_RED | FOREGROUND_INTENSITY;
        case Tone::Normal: return FOREGROUND_RED | FOREGROUND_GREEN | FOREGROUND_BLUE | FOREGROUND_INTENSITY;
        }
        return saved.wAttributes;
    }
    void fail(const char* reason) noexcept {
        pressed.clear();
        failure = reason;
        allow = active = false;
        if (cursor_saved && cursor_hidden) SetConsoleCursorInfo(out,&cursor);
        cursor_hidden = false;
        // Attempt one plain snapshot on the next normal runtime refresh.
        dirty = true;
    }
    void draw() {
        if (!model) return;
        CONSOLE_SCREEN_BUFFER_INFO info{};
        const bool console = output_saved && GetConsoleScreenBufferInfo(out,&info);
        const auto width = console ? static_cast<std::size_t>(info.srWindow.Right-info.srWindow.Left+1) : 100;
        const auto height = console ? static_cast<std::size_t>(info.srWindow.Bottom-info.srWindow.Top+1) : 0;
        const bool now_active = allow && console && width >= 72 && height >= 24;
        const bool moved = console && (viewport.Left != info.srWindow.Left || viewport.Top != info.srWindow.Top
            || viewport.Right != info.srWindow.Right || viewport.Bottom != info.srWindow.Bottom);
        if (!dirty && !moved && active == now_active) return;
        const bool invalidate = moved || active != now_active;
        if (console) viewport = info.srWindow;
        active = now_active;
        auto next = render(*model,width,height,page,active);
        page = next.page;
        if (!active) {
            if (cursor_saved && cursor_hidden) SetConsoleCursorInfo(out,&cursor);
            cursor_hidden = false;
            const auto text = plain_text(next);
            if (text != previous_plain) {
                DWORD written = 0;
                if (!WriteFile(out,text.data(),static_cast<DWORD>(text.size()),&written,nullptr) || written != text.size()) {
                    failure = "Console output failed; inspect the trainer log.";
                    dirty = false;
                    return;
                }
                previous_plain = text;
            }
            previous = {};
            dirty = false;
            return;
        }
        const auto batches = diff(previous,next,invalidate);
        if (!batches.empty() && !cursor_hidden) {
            auto hidden = cursor;
            hidden.bVisible = FALSE;
            if (!SetConsoleCursorInfo(out,&hidden)) { fail("Console cursor update failed; using plain output."); return; }
            cursor_hidden = true;
        }
        for (const auto& batch : batches) {
            const auto batch_width = std::min<std::size_t>(std::max(previous.width,next.width),width);
            if (batch.first >= height) continue;
            const auto count = std::min(batch.rows.size(),height-batch.first);
            std::vector<CHAR_INFO> cells(batch_width*count);
            for (std::size_t y=0; y<count; ++y) for (std::size_t x=0; x<batch_width; ++x) {
                auto& cell = cells[y*batch_width+x];
                cell.Char.AsciiChar = x<batch.rows[y].text.size() ? batch.rows[y].text[x] : ' ';
                cell.Attributes = attributes(batch.rows[y].tone);
            }
            SMALL_RECT region{viewport.Left,static_cast<SHORT>(viewport.Top+batch.first),
                static_cast<SHORT>(viewport.Left+batch_width-1),static_cast<SHORT>(viewport.Top+batch.first+count-1)};
            const auto expected = region;
            if (!WriteConsoleOutputA(out,cells.data(),{static_cast<SHORT>(batch_width),static_cast<SHORT>(count)},
                    {0,0},&region) || region.Left != expected.Left || region.Right != expected.Right
                    || region.Top != expected.Top || region.Bottom != expected.Bottom) {
                fail("Console redraw failed; using plain output."); return;
            }
        }
        previous = std::move(next);
        previous_plain.clear();
        dirty = false;
    }
    void navigation() {
        pressed.clear();
        if (!input_saved) return;
        // Console key records are delivered only to the focused console,
        // including Windows Terminal/ConPTY. Runtime global hotkeys continue
        // using their independent asynchronous rising-edge latches. A delivered
        // chord authorizes only that chord in this refresh, never future global
        // input. FOCUS_EVENT's field is reserved by Win32, so focus gain cannot
        // establish trustworthy tab/window eligibility.
        // This class must be the sole console input-record consumer.
        DWORD available = 0;
        if (!GetNumberOfConsoleInputEvents(in,&available)) {
            fail("Console input failed; using plain output."); return;
        }
        if (!available) return;
        INPUT_RECORD records[64]{};
        DWORD count = 0;
        if (!ReadConsoleInputW(in,records,std::min<DWORD>(available,64),&count)) {
            fail("Console input failed; using plain output."); return;
        }
        for (DWORD i=0; i<count; ++i) {
            const auto& record = records[i];
            if (record.EventType == FOCUS_EVENT) {
                up = down = false;
                // Reserved event: conservative cancellation, never authorization.
                pressed.clear();
            }
            if (record.EventType != KEY_EVENT) continue;
            const auto& key = record.Event.KeyEvent;
            const unsigned modifiers = ((key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED)) ? 0x100 : 0)
                | ((key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED)) ? 0x200 : 0)
                | ((key.dwControlKeyState & SHIFT_PRESSED) ? 0x400 : 0);
            if (key.bKeyDown) pressed.insert(key.wVirtualKeyCode | modifiers);
            else {
                for (auto it = pressed.begin(); it != pressed.end();) {
                    if ((*it & 0xff) == key.wVirtualKeyCode) it = pressed.erase(it);
                    else ++it;
                }
            }
            if (!active) continue;
            const bool modified = (key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED
                | LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED | SHIFT_PRESSED)) != 0;
            if (key.wVirtualKeyCode == VK_LEFT) {
                if (key.bKeyDown && !up && !modified && page) { --page; dirty = true; }
                up = key.bKeyDown != FALSE;
            }
            if (key.wVirtualKeyCode == VK_RIGHT) {
                if (key.bKeyDown && !down && !modified && page+1<previous.pages) { ++page; dirty = true; }
                down = key.bKeyDown != FALSE;
            }
        }
        // A later loss/key-up may still be queued. Do not authorize from a
        // partial batch; subsequent refreshes drain the backlog without waiting.
        if (!GetNumberOfConsoleInputEvents(in,&available) || available) pressed.clear();
    }
};
WindowsConsole::WindowsConsole(bool allow) noexcept {
    try { impl_ = std::make_unique<Impl>(allow); } catch (...) {}
}
WindowsConsole::~WindowsConsole() noexcept = default;
void WindowsConsole::update(const Model& model) noexcept {
    if (!impl_) return;
    try { impl_->model = model; impl_->dirty = true; impl_->draw(); }
    catch (...) { impl_->fail("Dashboard unavailable; inspect the trainer log."); }
}
void WindowsConsole::refresh() noexcept {
    if (!impl_) return;
    try { impl_->navigation(); impl_->draw(); }
    catch (...) { impl_->fail("Dashboard unavailable; inspect the trainer log."); }
}
bool WindowsConsole::hotkey_pressed(unsigned binding, bool async_rising, bool game_foreground) const noexcept {
    return binding && async_rising && (game_foreground || (impl_ && impl_->pressed.count(binding)));
}
void WindowsConsole::navigate(Navigation direction) noexcept {
    if (!impl_ || !impl_->active) return;
    if (direction == Navigation::Previous && impl_->page) --impl_->page;
    if (direction == Navigation::Next && impl_->page+1<impl_->previous.pages) ++impl_->page;
    impl_->dirty = true;
    refresh();
}
bool WindowsConsole::dashboard_active() const noexcept { return impl_ && impl_->active; }
const char* WindowsConsole::error() const noexcept {
    return impl_ ? impl_->failure : "Dashboard allocation failed; inspect the trainer log.";
}
}
