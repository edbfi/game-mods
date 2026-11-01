// SPDX-License-Identifier: AGPL-3.0-only
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include "metro/windows_console.hpp"
#include <windows.h>
#include <algorithm>
#include <optional>
#include <set>
#include <map>
#include <utility>

namespace metro::ui {
#ifdef METRO_CONSOLE_TESTING
namespace { bool test_foreground = false; }
void test_classic_foreground(bool foreground) noexcept { test_foreground = foreground; }
#endif
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
    struct Hold { bool pending = false, fired = false, blocked = false; };
    std::map<unsigned, Hold> holds;
    bool cancel_hold = false, input_complete = true, input_healthy = false;
    bool classic_foreground = false, classic_events = false;
    std::set<unsigned> record_down, classic_ready;
    std::optional<SHORT> dashboard_bottom;

    void finish_dashboard() noexcept {
        if (!dashboard_bottom) return;
        CONSOLE_SCREEN_BUFFER_INFO now{};
        if (GetConsoleScreenBufferInfo(out,&now)) {
            const auto bottom = std::min(*dashboard_bottom,static_cast<SHORT>(now.dwSize.Y-1));
            SetConsoleCursorPosition(out,{0,bottom});
            DWORD written = 0;
            // Advance/scroll even when the dashboard occupied the last buffer row.
            WriteConsoleA(out,"\r\n",2,&written,nullptr);
        }
        dashboard_bottom.reset();
    }

    explicit Impl(bool enabled) : allow(enabled) {
        output_saved = GetConsoleMode(out,&output_mode) != 0;
        input_saved = GetConsoleMode(in,&input_mode) != 0;
        cursor_saved = GetConsoleCursorInfo(out,&cursor) != 0;
        info_saved = GetConsoleScreenBufferInfo(out,&saved) != 0;
        // Preserve processed Ctrl+C and resize flags; the dashboard has no mouse UI.
        if (allow && output_saved && input_saved && info_saved && cursor_saved) {
            if (!SetConsoleMode(in,(input_mode | ENABLE_EXTENDED_FLAGS) & ~(ENABLE_QUICK_EDIT_MODE | ENABLE_MOUSE_INPUT))) {
                failure = "Console setup failed; using plain output.";
                allow = false;
            }
            // Classic conhost opens its system menu for Alt+Space before key
            // records reach us. Remove that menu for our console, then restore
            // the exact style on exit. This cannot control a ConPTY host's menu
            // or shortcut interception: GetConsoleWindow is hidden there.
            window_style = window ? GetWindowLongPtrW(window,GWL_STYLE) : 0;
            if (allow && (window_style & WS_SYSMENU)) {
                SetLastError(0);
                const auto old = SetWindowLongPtrW(window,GWL_STYLE,window_style & ~WS_SYSMENU);
                style_saved = old != 0 || GetLastError() == 0;
                if (!style_saved) failure = "Cannot disable console system menu; choose a master binding other than Alt+Space.";
            }
        } else allow = false;
        input_healthy = allow;
    }
    ~Impl() noexcept {
        if (style_saved) SetWindowLongPtrW(window,GWL_STYLE,window_style);
        if (input_saved) SetConsoleMode(in,input_mode);
        if (output_saved) SetConsoleMode(out,output_mode);
        if (info_saved) {
            SetConsoleTextAttribute(out,saved.wAttributes);
            finish_dashboard();
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
        holds.clear();
        record_down.clear();
        classic_ready.clear();
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
        if (!dirty && !(moved && now_active) && active == now_active) return;
        const bool invalidate = (moved && now_active) || active != now_active;
        if (console) viewport = info.srWindow;
        active = now_active;
        // Interactive plain output keeps stable logical rows so a toggle emits
        // only changed state/status lines. The console itself wraps long lines.
        auto next = render(*model,console && !active ? 160 : width,height,page,active);
        page = next.page;
        if (!active) {
            if (cursor_saved && cursor_hidden) SetConsoleCursorInfo(out,&cursor);
            cursor_hidden = false;
            finish_dashboard();
            std::string text;
            for (std::size_t i=0; i<next.rows.size(); ++i) {
                if (!console || invalidate || i>=previous.rows.size() || !(previous.rows[i] == next.rows[i]))
                    text += next.rows[i].text + '\n';
            }
            if (!console) text = plain_text(next);
            if (!text.empty() && (console || text != previous_plain)) {
                DWORD written = 0;
                if (!WriteFile(out,text.data(),static_cast<DWORD>(text.size()),&written,nullptr) || written != text.size()) {
                    failure = "Console output failed; inspect the trainer log.";
                    dirty = false;
                    return;
                }
                previous_plain = text;
            }
            previous = std::move(next);
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
        auto content_rows = next.rows.size();
        while (content_rows && next.rows[content_rows-1].text.empty()) --content_rows;
        dashboard_bottom = static_cast<SHORT>(viewport.Top + (content_rows ? content_rows-1 : 0));
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
    bool is_classic_foreground() const noexcept {
#ifdef METRO_CONSOLE_TESTING
        return test_foreground;
#else
        return window && IsWindowVisible(window) && GetForegroundWindow() == window;
#endif
    }
    void fail_input() noexcept {
        input_healthy = false;
        fail("Console input failed; using plain output.");
    }
    void navigation() {
        cancel_hold = false;
        input_complete = true;
        classic_ready.clear();
        const bool foreground = is_classic_foreground();
        // Drain the first batch after entering the classic window without event-only
        // authorization: it may predate the focus change. ConPTY never uses this path.
        classic_events = foreground && classic_foreground;
        classic_foreground = foreground;
        if (!input_healthy) return;
        // Async edges may precede records or arrive in the same poll, but must
        // remain in the same physical hold. Focus records only cancel.
        DWORD available = 0;
        if (!GetNumberOfConsoleInputEvents(in,&available)) {
            fail_input(); return;
        }
        if (!available) return;
        INPUT_RECORD records[64]{};
        DWORD count = 0;
        if (!ReadConsoleInputW(in,records,std::min<DWORD>(available,64),&count)) {
            fail_input(); return;
        }
        for (DWORD i=0; i<count; ++i) {
            const auto& record = records[i];
            if (record.EventType == FOCUS_EVENT) {
                up = down = false;
                // Reserved event: conservative cancellation, never authorization.
                pressed.clear();
                classic_ready.clear();
                classic_events = false;
                classic_foreground = false;
                cancel_hold = true;
                for (auto& hold : holds) hold.second = {false,false,true};
            }
            if (record.EventType != KEY_EVENT) continue;
            const auto& key = record.Event.KeyEvent;
            const unsigned modifiers = ((key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED)) ? 0x100 : 0)
                | ((key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED)) ? 0x200 : 0)
                | ((key.dwControlKeyState & SHIFT_PRESSED) ? 0x400 : 0);
            // A modifier change cancels an unfinished event chord, but not a
            // completed tap followed by the modifier's release in this batch.
            for (auto it=classic_ready.begin(); it!=classic_ready.end();) {
                if (record_down.count(*it & 0xff) && (*it & 0x700) != modifiers)
                    it = classic_ready.erase(it);
                else ++it;
            }
            // Cancel existing holds on modifier changes; unrelated typing must
            // not pre-block a binding that has not started a hold yet.
            for (auto& hold : holds) {
                const bool tracking = hold.second.pending || hold.second.fired
                    || hold.second.blocked || pressed.count(hold.first);
                if (tracking && ((hold.first & 0x700) != modifiers ||
                    (!key.bKeyDown && (hold.first & 0xff) == key.wVirtualKeyCode)))
                    hold.second = {false,false,true};
            }
            for (auto it=pressed.begin(); it!=pressed.end();) {
                if ((*it & 0x700) != modifiers) it = pressed.erase(it);
                else ++it;
            }
            if (key.bKeyDown) {
                const auto binding = key.wVirtualKeyCode | modifiers;
                const bool fresh = record_down.insert(key.wVirtualKeyCode).second;
                const auto hold = holds.find(binding);
                const bool blocked = hold != holds.end() && (hold->second.blocked || hold->second.fired);
                if (fresh && classic_events && !cancel_hold && !blocked) classic_ready.insert(binding);
                pressed.insert(binding);
            }
            else {
                record_down.erase(key.wVirtualKeyCode);
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
        if (!GetNumberOfConsoleInputEvents(in,&available)) { fail_input(); return; }
        input_complete = !available;
        if (!input_complete) {
            classic_foreground = false;
            classic_ready.clear();
            pressed.clear();
            for (auto& hold : holds) hold.second = {false,false,true};
        }
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
bool WindowsConsole::hotkey_pressed(unsigned binding, bool down, bool async_rising, bool game_foreground) noexcept {
    if (!binding || !impl_) return binding && async_rising && game_foreground;
    try {
        auto& hold = impl_->holds[binding];
        const bool console_event = impl_->classic_ready.erase(binding) != 0;
        if (console_event && impl_->input_healthy && impl_->classic_events
                && impl_->is_classic_foreground() && !game_foreground
                && !hold.fired) {
            hold = {};
            hold.fired = down;
            return true;
        }
        if (!down) {
            hold = {};
            impl_->pressed.erase(binding);
            return false;
        }
        if (game_foreground) {
            hold.pending = false;
            hold.fired = hold.fired || async_rising;
            return async_rising;
        }
        if (!impl_->input_healthy) return false;
        if (impl_->cancel_hold || !impl_->input_complete) hold.blocked = true;
        if (async_rising && !hold.blocked) hold.pending = true;
        if (!hold.pending || hold.fired || hold.blocked || !impl_->pressed.count(binding)) return false;
        hold.pending = false;
        hold.fired = true;
        impl_->pressed.erase(binding);
        return true;
    } catch (...) {
        impl_->fail("Console hotkey tracking failed; inspect the trainer log.");
        return async_rising && game_foreground;
    }
}
bool WindowsConsole::dashboard_active() const noexcept { return impl_ && impl_->active; }
const char* WindowsConsole::error() const noexcept {
    return impl_ ? impl_->failure : "Dashboard allocation failed; inspect the trainer log.";
}
}
