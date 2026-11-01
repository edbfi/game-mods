// SPDX-License-Identifier: AGPL-3.0-only
#define NOMINMAX
#include "metro/windows_console.hpp"
#include <windows.h>
#include <iostream>
#include <stdexcept>

namespace {
void check(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }
}
int main() {
    try {
        // Only our disposable test process changes console state.
        FreeConsole();
        check(AllocConsole() != 0,"allocate test console");
        // WSL inherits redirected standard handles even after AllocConsole.
        const auto in = CreateFileW(L"CONIN$",GENERIC_READ | GENERIC_WRITE,FILE_SHARE_READ | FILE_SHARE_WRITE,nullptr,OPEN_EXISTING,0,nullptr);
        const auto out = CreateFileW(L"CONOUT$",GENERIC_READ | GENERIC_WRITE,FILE_SHARE_READ | FILE_SHARE_WRITE,nullptr,OPEN_EXISTING,0,nullptr);
        check(in != INVALID_HANDLE_VALUE && out != INVALID_HANDLE_VALUE,"open dedicated console handles");
        SetStdHandle(STD_INPUT_HANDLE,in);
        SetStdHandle(STD_OUTPUT_HANDLE,out);
        DWORD mode = 0;
        check(GetConsoleMode(in,&mode) != 0,"read input mode");
        check(SetConsoleMode(in,mode | ENABLE_MOUSE_INPUT | ENABLE_WINDOW_INPUT | ENABLE_QUICK_EDIT_MODE
            | ENABLE_EXTENDED_FLAGS),"seed mouse, resize and QuickEdit modes");
        check(GetConsoleMode(in,&mode),"read seeded mode");
        const auto style = GetWindowLongPtrW(GetConsoleWindow(),GWL_STYLE);
        CONSOLE_CURSOR_INFO cursor{};
        check(GetConsoleCursorInfo(out,&cursor) != 0,"read cursor");
        const SMALL_RECT viewport{0,0,99,29};
        check(SetConsoleScreenBufferSize(out,{120,60}) != 0,"size test buffer");
        auto region = viewport;
        check(SetConsoleWindowInfo(out,TRUE,&region) != 0,"size test viewport");
        {
            metro::ui::WindowsConsole console;
            metro::ui::Model model;
            model.config = metro::parse_config("");
            model.lifecycle = metro::ui::Lifecycle::Attached;
            model.master = true;
            model.logfile = "C:/test/metro-trainer.log";
            console.update(model);
            check(console.dashboard_active(),"dashboard active at 100x30");
            check(!console.error(),"console setup and draw succeeded");
            CHAR_INFO masthead{};
            SMALL_RECT cell{3,0,3,0};
            check(ReadConsoleOutputA(out,&masthead,{1,1},{0,0},&cell) != 0,"read masthead color");
            check(masthead.Attributes == (FOREGROUND_RED | FOREGROUND_GREEN | FOREGROUND_INTENSITY),"amber masthead");
            DWORD current = 0;
            check(GetConsoleMode(in,&current) && (current & ENABLE_PROCESSED_INPUT) == (mode & ENABLE_PROCESSED_INPUT),"processed Ctrl+C preserved");
            check(!(current & (ENABLE_MOUSE_INPUT | ENABLE_QUICK_EDIT_MODE)),"mouse and QuickEdit disabled");
            check((current & ENABLE_WINDOW_INPUT) == (mode & ENABLE_WINDOW_INPUT),"resize mode preserved");
            check(!(GetWindowLongPtrW(GetConsoleWindow(),GWL_STYLE) & WS_SYSMENU),"Alt+Space system menu disabled");
            char before[100]{}, after[100]{};
            DWORD done = 0;
            check(ReadConsoleOutputCharacterA(out,before,100,{0,14},&done) && done == 100,"read first weapon row");
            FlushConsoleInputBuffer(in);
            INPUT_RECORD keys[2]{};
            for (auto& key : keys) { key.EventType = KEY_EVENT; key.Event.KeyEvent.wVirtualKeyCode = VK_RIGHT; }
            keys[0].Event.KeyEvent.bKeyDown = TRUE;
            check(WriteConsoleInputW(in,keys,2,&done) && done == 2,"queue navigation");
            console.refresh();
            check(ReadConsoleOutputCharacterA(out,after,100,{0,14},&done) && done == 100,"read next weapon row");
            check(std::string(before,100) != std::string(after,100),"console record pages weapon rows");
            DWORD available = 0;
            check(GetNumberOfConsoleInputEvents(in,&available) && available == 0,"UI consumes queued input");
            metro::ui::test_classic_foreground(false);
            console.refresh();
            const auto chord = metro::parse_hotkey("CTRL+1");
            const auto second = metro::parse_hotkey("CTRL+2");
            auto queue = [&](WORD type, bool down, DWORD modifiers = LEFT_CTRL_PRESSED, WORD vk = '1') {
                INPUT_RECORD record{};
                record.EventType = type;
                if (type == KEY_EVENT) {
                    record.Event.KeyEvent.wVirtualKeyCode = vk;
                    record.Event.KeyEvent.wVirtualScanCode = static_cast<WORD>(MapVirtualKeyW(vk,MAPVK_VK_TO_VSC));
                    record.Event.KeyEvent.uChar.UnicodeChar = static_cast<wchar_t>(vk);
                    record.Event.KeyEvent.bKeyDown = down;
                    record.Event.KeyEvent.dwControlKeyState = modifiers;
                    record.Event.KeyEvent.wRepeatCount = 1;
                } else record.Event.FocusEvent.bSetFocus = down;
                check(WriteConsoleInputW(in,&record,1,&done) && done == 1,"queue shortcut evidence");
            };
            auto release = [&]() {
                queue(KEY_EVENT,false);
                queue(KEY_EVENT,false,LEFT_CTRL_PRESSED,'2');
                console.refresh();
                console.hotkey_pressed(chord,false,false,false);
                console.hotkey_pressed(second,false,false,false);
            };
            check(!console.hotkey_pressed(chord,true,true,false),"unrelated background edge waits for exact evidence");
            // Several empty polls must not eat a pending edge during a hold.
            for (int i=0; i<3; ++i) {
                console.refresh();
                check(!console.hotkey_pressed(chord,true,false,false),"delayed record still pending");
            }
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,false,false),"delayed record meets earlier edge");
            queue(KEY_EVENT,true); // autorepeat
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"repeat record never duplicates toggle");
            release();
            metro::KeyLatch latch;
            check(!latch.update(false),"latch starts released");
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,latch.update(true),false),"same-poll edge and record fire");
            check(!console.hotkey_pressed(chord,true,latch.update(true),false),"real held latch has no new edge");
            release();
            check(!latch.update(false),"latch release is not a rising edge");
            check(console.hotkey_pressed(chord,true,true,true),"game retains async path without console records");
            check(!console.hotkey_pressed(chord,true,false,true),"game hold never repeats");
            release();
            check(!console.hotkey_pressed(0,true,true,true),"unbound shortcut stays disabled");
            // Both edges are pending; presentation while handling the first
            // binding must leave the second binding's queued record untouched.
            check(!console.hotkey_pressed(chord,true,true,false),"first edge pending");
            check(!console.hotkey_pressed(second,true,true,false),"second edge pending");
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,false,false),"first binding fires");
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED,'2');
            console.update(model);
            check(GetNumberOfConsoleInputEvents(in,&available) && available == 1,"presentation does not drain second binding");
            console.refresh();
            check(console.hotkey_pressed(second,true,false,false),"second binding survives presentation between polls");
            check(!console.hotkey_pressed(chord,true,false,false),"first binding remains consumed");
            release();
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED,'2');
            console.refresh();
            check(console.hotkey_pressed(chord,true,true,false) && console.hotkey_pressed(second,true,true,false),"simultaneous bindings each fire once");
            console.update(model);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false) && !console.hotkey_pressed(second,true,false,false),"simultaneous holds never repeat");
            release();
            queue(KEY_EVENT,true,0,'A');
            queue(KEY_EVENT,false,0,'A');
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,true,false),"unrelated typing does not block a subsequent fresh chord");
            release();
            for (const auto focus : {false,true}) {
                console.hotkey_pressed(chord,true,true,false);
                queue(KEY_EVENT,true);
                queue(FOCUS_EVENT,focus);
                console.refresh();
                check(!console.hotkey_pressed(chord,true,false,false),"focus transition cancels pending edge");
                queue(KEY_EVENT,true);
                console.refresh();
                check(!console.hotkey_pressed(chord,true,false,false),"held chord cannot reactivate after focus transition");
                release();
            }
            console.hotkey_pressed(chord,true,true,false);
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"queued release cancels earlier edge and record");
            release();
            console.hotkey_pressed(chord,true,true,false);
            console.hotkey_pressed(chord,false,false,false);
            queue(KEY_EVENT,true);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"late record after physical release cannot fire");
            console.hotkey_pressed(chord,true,true,false);
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED | SHIFT_PRESSED);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"modifier change cancels pending edge");
            release();
            queue(KEY_EVENT,true);
            for (unsigned i=0; i<64; ++i) queue(FOCUS_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,true,false),"partial batch cannot hide queued focus loss");
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"drained focus loss cannot revive pending edge");
            release();
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"background paired tap rejected");
            metro::ui::test_classic_foreground(true);
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"focus-entry stale pair rejected");
            console.refresh();
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            console.refresh();
            check(console.hotkey_pressed(chord,false,false,false),"classic short tap needs no async edge");
            check(!console.hotkey_pressed(chord,false,false,false),"classic tap consumed once");
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            queue(KEY_EVENT,false,0,VK_CONTROL);
            console.refresh();
            check(console.hotkey_pressed(chord,false,false,false),"completed tap survives subsequent modifier release");
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,true,false),"classic full hold fires once");
            queue(KEY_EVENT,true);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"classic autorepeat suppressed");
            check(!console.hotkey_pressed(chord,true,false,true),"console to game transition cannot duplicate hold");
            release();
            check(console.hotkey_pressed(chord,true,true,true),"game edge starts a new hold");
            queue(KEY_EVENT,true);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false,false),"game to console transition cannot duplicate hold");
            release();
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED,'2');
            queue(KEY_EVENT,false,LEFT_CTRL_PRESSED,'2');
            console.refresh();
            check(console.hotkey_pressed(chord,false,false,false)
                && console.hotkey_pressed(second,false,false,false),"classic multiple paired bindings");
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED | SHIFT_PRESSED);
            queue(KEY_EVENT,false,LEFT_CTRL_PRESSED | SHIFT_PRESSED);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"classic modifiers must match exactly");
            queue(KEY_EVENT,true);
            queue(FOCUS_EVENT,false);
            queue(KEY_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"focus boundary cancels classic tap");
            release();
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED | SHIFT_PRESSED,VK_SHIFT);
            queue(KEY_EVENT,false,LEFT_CTRL_PRESSED | SHIFT_PRESSED);
            console.refresh();
            check(!console.hotkey_pressed(chord,false,false,false),"modifier change cancels unfinished classic chord");
            release();
            model.error = "Restoration incomplete. Close the game before restarting trainer.";
            console.update(model);
            console.refresh();
            check(!console.error(),"error redraw succeeds");
            char diagnostic[100]{};
            check(ReadConsoleOutputCharacterA(out,diagnostic,100,{0,27},&done) != 0,"read recovery action");
            check(std::string(diagnostic,100).find("Close the game") != std::string::npos,"persistent recovery action visible");
            // Small interactive output emits changed rows; expansion restores paging.
            SMALL_RECT small{0,0,69,19};
            check(SetConsoleWindowInfo(out,TRUE,&small) != 0,"shrink viewport");
            console.refresh();
            check(!console.dashboard_active(),"small viewport fallback");
            check(SetConsoleScreenBufferSize(out,{120,200}),"leave space to measure compact output");
            CONSOLE_SCREEN_BUFFER_INFO small_before{}, small_after{};
            check(GetConsoleScreenBufferInfo(out,&small_before),"read narrow output position");
            console.refresh();
            console.update(model);
            check(GetConsoleScreenBufferInfo(out,&small_after)
                && small_before.dwCursorPosition.Y == small_after.dwCursorPosition.Y
                && small_before.dwCursorPosition.X == small_after.dwCursorPosition.X,"unchanged narrow output writes nothing");
            model.status = "Narrow status changed.";
            console.update(model);
            check(GetConsoleScreenBufferInfo(out,&small_after)
                && small_after.dwCursorPosition.Y == small_before.dwCursorPosition.Y+1,"narrow status change emits one line, not a snapshot");
            release();
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,true,false),"console hotkeys survive plain small viewport");
            check(SetConsoleWindowInfo(out,TRUE,&region) != 0,"expand viewport");
            console.refresh();
            check(console.dashboard_active() && !console.error(),"resize returns to dashboard");
        }
        DWORD restored = 0;
        CONSOLE_CURSOR_INFO restored_cursor{};
        check(GetConsoleMode(in,&restored) && restored == mode,"input mode restored");
        check(GetConsoleCursorInfo(out,&restored_cursor) && restored_cursor.bVisible == cursor.bVisible && restored_cursor.dwSize == cursor.dwSize,"cursor restored");
        check(GetWindowLongPtrW(GetConsoleWindow(),GWL_STYLE) == style,"system menu restored");
        CONSOLE_SCREEN_BUFFER_INFO final_info{};
        check(GetConsoleScreenBufferInfo(out,&final_info) && final_info.dwCursorPosition.X == 0
            && final_info.dwCursorPosition.Y == 28,"next shell prompt starts below dashboard content");
        DWORD output_mode = 0;
        check(GetConsoleMode(out,&output_mode),"read output mode");
        for (const auto vt : {false,true}) {
            check(SetConsoleMode(out,vt ? output_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                : output_mode & ~ENABLE_VIRTUAL_TERMINAL_PROCESSING),"set classic/VT output");
            {
                metro::ui::WindowsConsole console;
                metro::ui::Model model;
                console.update(model);
                check(console.dashboard_active(),"classic/VT dashboard draws");
            }
            check(GetConsoleScreenBufferInfo(out,&final_info) && final_info.dwCursorPosition.X == 0
                && final_info.dwCursorPosition.Y == final_info.srWindow.Top+28,"classic/VT prompt below final status");
            char tail[100]{};
            DWORD read = 0;
            check(ReadConsoleOutputCharacterA(out,tail,100,final_info.dwCursorPosition,&read) && read == 100
                && std::string(tail,100) == std::string(100,' '),"classic/VT prompt row has no dashboard leftovers");
        }
        check(SetConsoleMode(out,output_mode),"restore test output mode");
        HANDLE broken = nullptr;
        check(DuplicateHandle(GetCurrentProcess(),in,GetCurrentProcess(),&broken,0,FALSE,DUPLICATE_SAME_ACCESS),"duplicate input");
        SetStdHandle(STD_INPUT_HANDLE,broken);
        {
            metro::ui::WindowsConsole console;
            metro::ui::Model model;
            console.update(model);
            check(console.dashboard_active(),"failure test starts healthy");
            metro::ui::test_classic_foreground(false);
            console.refresh();
            check(!console.hotkey_pressed(metro::parse_hotkey("CTRL+2"),true,true,false),"pending edge before reader failure");
            CloseHandle(broken);
            console.refresh();
            check(console.error() && !console.dashboard_active(),"invalid input switches to plain output");
            const std::string diagnostic = console.error();
            CONSOLE_SCREEN_BUFFER_INFO before{}, after{};
            check(GetConsoleScreenBufferInfo(out,&before),"read fallback position");
            for (int i=0; i<5; ++i) console.refresh();
            check(GetConsoleScreenBufferInfo(out,&after) && before.dwCursorPosition.X == after.dwCursorPosition.X
                && before.dwCursorPosition.Y == after.dwCursorPosition.Y,"failed reader does not dirty fallback repeatedly");
            check(console.error() && diagnostic == console.error(),"input diagnostic persists");
            check(console.hotkey_pressed(metro::parse_hotkey("CTRL+1"),true,true,true),"game hotkeys survive input failure");
            check(!console.hotkey_pressed(metro::parse_hotkey("CTRL+2"),true,true,false),"failed input cannot authorize console hotkeys");
        }
        SetStdHandle(STD_INPUT_HANDLE,in);
        CloseHandle(in); CloseHandle(out);
        FreeConsole();
        std::cout << "Windows console checks passed\n";
        return 0;
    } catch (const std::exception& error) {
        FreeConsole();
        std::cerr << error.what() << '\n';
        return 1;
    }
}
