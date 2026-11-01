// SPDX-License-Identifier: AGPL-3.0-only
// Runs build/metro-trainer.exe against fake metro.exe processes (copies of windows-backend-tests.exe in
// --child mode) staged outside any Steam folder. Read-only modes only: --check and --stop.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <tlhelp32.h>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
void require(bool ok, const std::string& message) { if (!ok) throw std::runtime_error(message); }
std::string utf8(const std::wstring& text) {
    if (text.empty()) return {};
    const int size = WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0, nullptr, nullptr);
    std::string result(static_cast<std::size_t>(size), '\0');
    WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), result.data(), size, nullptr, nullptr);
    return result;
}
std::wstring directory_of(const std::wstring& path) { return path.substr(0, path.find_last_of(L"\\/")); }
bool metro_running() {
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    require(snapshot != INVALID_HANDLE_VALUE, "Cannot enumerate processes.");
    PROCESSENTRY32W entry{}; entry.dwSize = sizeof entry;
    bool found = false;
    if (Process32FirstW(snapshot, &entry)) do {
        if (_wcsicmp(entry.szExeFile, L"metro.exe") == 0) found = true;
    } while (!found && Process32NextW(snapshot, &entry));
    CloseHandle(snapshot);
    return found;
}
// A fresh folder under %TEMP%, by its long name: the trainer logs the long path Windows reports.
std::wstring make_temp_folder(const wchar_t* prefix) {
    wchar_t base[MAX_PATH + 1]{};
    require(GetTempPathW(MAX_PATH + 1, base) > 0, "Cannot locate the temp folder.");
    for (unsigned attempt = 0; attempt < 100; ++attempt) {
        const auto path = std::wstring(base) + prefix + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(attempt);
        if (CreateDirectoryW(path.c_str(), nullptr)) {
            std::wstring full(32768, L'\0');
            const auto length = GetLongPathNameW(path.c_str(), full.data(), static_cast<DWORD>(full.size()));
            require(length > 0 && length < full.size(), "Cannot resolve the temp folder.");
            full.resize(length);
            return full;
        }
        require(GetLastError() == ERROR_ALREADY_EXISTS, "Cannot create a temp folder.");
    }
    throw std::runtime_error("Cannot create a temp folder.");
}
struct Folder {
    std::wstring path;
    std::vector<std::wstring> files;
    ~Folder() {
        for (const auto& file : files) DeleteFileW(file.c_str());
        DeleteFileW((path + L"\\metro-trainer.log").c_str());
        RemoveDirectoryW(path.c_str());
    }
    std::wstring copy(const std::wstring& source, const wchar_t* name) {
        const auto target = path + L"\\" + name;
        require(CopyFileW(source.c_str(), target.c_str(), TRUE), "Cannot stage " + utf8(target));
        files.push_back(target);
        return target;
    }
};
struct Fake {
    PROCESS_INFORMATION info{};
    explicit Fake(const std::wstring& exe) {
        std::wstring command = L"\"" + exe + L"\" --child";
        STARTUPINFOW startup{}; startup.cb = sizeof startup;
        require(CreateProcessW(exe.c_str(), command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
                               nullptr, nullptr, &startup, &info), "Cannot start a fake metro.exe.");
    }
    ~Fake() {
        TerminateProcess(info.hProcess, 0);
        WaitForSingleObject(info.hProcess, 5000);
        CloseHandle(info.hThread); CloseHandle(info.hProcess);
    }
    Fake(const Fake&) = delete;
    Fake& operator=(const Fake&) = delete;
};
struct Result { DWORD code; std::string output; };
// Read-only commands keep plain diagnostics; normal runtime emits state snapshots.
Result run_trainer(const std::wstring& exe, const wchar_t* argument) {
    SECURITY_ATTRIBUTES security{sizeof security, nullptr, TRUE};
    HANDLE read = nullptr, write = nullptr;
    // Large enough for every line the trainer prints here, so it never blocks on a full pipe.
    require(CreatePipe(&read, &write, &security, 65536), "Cannot create a pipe.");
    SetHandleInformation(read, HANDLE_FLAG_INHERIT, 0);
    std::wstring command = L"\"" + exe + L"\" " + argument;
    STARTUPINFOW startup{}; startup.cb = sizeof startup; startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdOutput = write; startup.hStdError = write; startup.hStdInput = nullptr;
    PROCESS_INFORMATION info{};
    const bool started = CreateProcessW(exe.c_str(), command.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW,
                                        nullptr, directory_of(exe).c_str(), &startup, &info);
    CloseHandle(write);
    if (!started) { CloseHandle(read); throw std::runtime_error("Cannot start the trainer."); }
    // A trainer that hangs fails the test instead of hanging it; ending it closes the pipe.
    const bool exited = WaitForSingleObject(info.hProcess, 30000) == WAIT_OBJECT_0;
    if (!exited) { TerminateProcess(info.hProcess, 2); WaitForSingleObject(info.hProcess, 5000); }
    std::string output;
    char buffer[4096]; DWORD done = 0;
    while (ReadFile(read, buffer, sizeof buffer, &done, nullptr) && done) output.append(buffer, done);
    CloseHandle(read);
    DWORD code = 0;
    GetExitCodeProcess(info.hProcess, &code);
    CloseHandle(info.hThread); CloseHandle(info.hProcess);
    require(exited, "The trainer did not exit.");
    return {code, output};
}
void expect(const char* name, const Result& result, DWORD code, const std::vector<std::string>& texts) {
    bool ok = result.code == code;
    for (const auto& text : texts) ok = ok && result.output.find(text) != std::string::npos;
    require(ok, std::string(name) + ": expected exit " + std::to_string(code) + ", got " + std::to_string(result.code)
        + ". Output:\n" + result.output);
    std::cout << "PASS " << name << '\n';
}
}

int wmain() {
    try {
        require(!metro_running(), "A metro.exe is already running; close it before this test.");
        std::wstring self(32768, L'\0');
        const auto size = GetModuleFileNameW(nullptr, self.data(), static_cast<DWORD>(self.size()));
        require(size > 0 && size < self.size(), "Cannot locate the test executable.");
        self.resize(size);
        const auto build = directory_of(self);
        Folder trainer_folder{make_temp_folder(L"metro-trainer-test-"), {}};
        Folder game_folder{make_temp_folder(L"metro-game-test-"), {}};
        const auto trainer = trainer_folder.copy(build + L"\\metro-trainer.exe", L"metro-trainer.exe");
        trainer_folder.copy(directory_of(build) + L"\\config\\metro-trainer.ini", L"metro-trainer.ini");
        const auto game = game_folder.copy(build + L"\\windows-backend-tests.exe", L"metro.exe");

        expect("no game, --check", run_trainer(trainer, L"--check"), 1, {"Game is not running"});
        {
            Fake fake(game);
            expect("fake game from another folder, --check", run_trainer(trainer, L"--check"), 1,
                   {"Game: " + utf8(game), "Unsupported executable build; no memory was changed."});
            const auto normal = run_trainer(trainer, L"");
            expect("fake game, dashboard fallback", normal, 1,
                   {"edbfi / Metro", "Unsupported executable build", "metro-trainer.log", "Alt+Space"});
            require(normal.output.find("Game SHA-256:") == std::string::npos,
                    "Normal runtime must send routine log events only to file.");
            expect("fake game, --stop without a trainer", run_trainer(trainer, L"--stop"), 1,
                   {"No active trainer stop event"});
            Fake second(game);
            expect("two fake games, --check", run_trainer(trainer, L"--check"), 1, {"Multiple metro.exe processes"});
        }
        std::cout << "Trainer attach checks passed.\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
