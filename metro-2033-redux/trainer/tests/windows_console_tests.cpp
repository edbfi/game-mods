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
            const auto chord = metro::parse_hotkey("CTRL+1");
            auto queue = [&](WORD type, bool down, DWORD modifiers = LEFT_CTRL_PRESSED) {
                INPUT_RECORD record{};
                record.EventType = type;
                if (type == KEY_EVENT) {
                    record.Event.KeyEvent.wVirtualKeyCode = '1';
                    record.Event.KeyEvent.bKeyDown = down;
                    record.Event.KeyEvent.dwControlKeyState = modifiers;
                    record.Event.KeyEvent.wRepeatCount = 1;
                } else record.Event.FocusEvent.bSetFocus = down;
                check(WriteConsoleInputW(in,&record,1,&done) && done == 1,"queue shortcut evidence");
            };
            check(!console.hotkey_pressed(chord,true,false),"unrelated background edge rejected");
            check(console.hotkey_pressed(chord,true,true),"game foreground retains async hotkeys without console records");
            queue(FOCUS_EVENT,true);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"focus gain alone never authorizes terminal host");
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,false),"trainer terminal record authorizes exact chord without host HWND");
            check(!console.hotkey_pressed(chord,false,false),"held async key never repeats");
            check(!console.hotkey_pressed(metro::parse_hotkey("ALT+1"),true,false),"modifiers match exactly");
            check(!console.hotkey_pressed(0,true,true),"unbound shortcut stays disabled");
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"another tab in same host cannot reuse earlier evidence");
            queue(KEY_EVENT,true);
            queue(FOCUS_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"queued focus transition cancels earlier evidence");
            queue(KEY_EVENT,true);
            queue(KEY_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"queued release cancels earlier chord");
            queue(KEY_EVENT,true,LEFT_CTRL_PRESSED | SHIFT_PRESSED);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"extra modifier rejects chord");
            queue(KEY_EVENT,true);
            for (unsigned i=0; i<64; ++i) queue(FOCUS_EVENT,false);
            console.refresh();
            check(!console.hotkey_pressed(chord,true,false),"partial batch cannot hide queued loss of focus");
            console.refresh();
            model.error = "Restoration incomplete. Close the game before restarting trainer.";
            console.update(model);
            console.refresh();
            check(!console.error(),"error redraw succeeds");
            char diagnostic[100]{};
            check(ReadConsoleOutputCharacterA(out,diagnostic,100,{0,27},&done) != 0,"read recovery action");
            check(std::string(diagnostic,100).find("Close the game") != std::string::npos,"persistent recovery action visible");
            // A small window uses complete snapshots; expanding restores paging.
            SMALL_RECT small{0,0,69,19};
            check(SetConsoleWindowInfo(out,TRUE,&small) != 0,"shrink viewport");
            console.refresh();
            check(!console.dashboard_active(),"small viewport fallback");
            queue(KEY_EVENT,true);
            console.refresh();
            check(console.hotkey_pressed(chord,true,false),"console hotkeys survive plain small viewport");
            check(SetConsoleWindowInfo(out,TRUE,&region) != 0,"expand viewport");
            console.refresh();
            check(console.dashboard_active() && !console.error(),"resize returns to dashboard");
        }
        DWORD restored = 0;
        CONSOLE_CURSOR_INFO restored_cursor{};
        check(GetConsoleMode(in,&restored) && restored == mode,"input mode restored");
        check(GetConsoleCursorInfo(out,&restored_cursor) && restored_cursor.bVisible == cursor.bVisible && restored_cursor.dwSize == cursor.dwSize,"cursor restored");
        check(GetWindowLongPtrW(GetConsoleWindow(),GWL_STYLE) == style,"system menu restored");
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
