#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include "metro/windows_patch.hpp"
#include "metro/windows_no_reload.hpp"
#include <cstdio>
#include <cstring>
#include <iostream>
#include <string>

int wmain(int argc, wchar_t**) {
    if (argc == 2) {
        auto* page = static_cast<unsigned char*>(VirtualAlloc(nullptr, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE));
        if (!page) return 1;
        const unsigned char code[] = {0xb8, 1, 0, 0, 0, 0xc3};
        memcpy(page, code, sizeof code);
        DWORD old = 0;
        if (!VirtualProtect(page, 4096, PAGE_EXECUTE_READ, &old)) return 1;
        FlushInstructionCache(GetCurrentProcess(), page, sizeof code);
        std::printf("%llx\n", static_cast<unsigned long long>(reinterpret_cast<std::uintptr_t>(page)));
        std::fflush(stdout);
        auto function = reinterpret_cast<int(*)()>(page);
        for (;;) { volatile int result = function(); (void)result; Sleep(1); }
    }
    SECURITY_ATTRIBUTES security{sizeof security, nullptr, TRUE};
    HANDLE input = nullptr, output = nullptr;
    if (!CreatePipe(&input, &output, &security, 0)) return 1;
    SetHandleInformation(input, HANDLE_FLAG_INHERIT, 0);
    wchar_t path[32768]{};
    GetModuleFileNameW(nullptr, path, 32768);
    std::wstring command = L"\"" + std::wstring(path) + L"\" --child";
    STARTUPINFOW startup{}; startup.cb = sizeof startup; startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdOutput = output; startup.hStdError = output; startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    PROCESS_INFORMATION child{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE, 0, nullptr, nullptr, &startup, &child)) return 1;
    CloseHandle(output);
    int result = 1;
    try {
        std::string address;
        char ch = 0; DWORD done = 0;
        while (ReadFile(input, &ch, 1, &done, nullptr) && done && ch != '\n') address.push_back(ch);
        const auto target = static_cast<std::uintptr_t>(std::stoull(address, nullptr, 16));
        // Exercise the actual cave allocator against this disposable child.
        metro::NoReloadRuntime reload(child.hProcess, target-metro::no_reload_hook_rva, metro::verified_no_reload_classes());
        if (reload.hooks().size() != 17 || reload.code_ranges().size() != 17 || reload.current_controls() != 0)
            throw std::runtime_error("Incomplete or initially enabled persistent hook set");
        for (const auto& cave : reload.code_ranges()) {
            MEMORY_BASIC_INFORMATION cave_info{};
            if (!VirtualQueryEx(child.hProcess, reinterpret_cast<LPCVOID>(cave.address), &cave_info, sizeof cave_info)
                || cave_info.Protect != PAGE_EXECUTE_READ) throw std::runtime_error("Cave is not RX");
        }
        std::uintptr_t retained_control = 0;
        {
            metro::SharedControls controls(child.hProcess);
            retained_control = controls.remote_address();
            MEMORY_BASIC_INFORMATION control_info{};
            if (!VirtualQueryEx(child.hProcess, reinterpret_cast<LPCVOID>(retained_control), &control_info, sizeof control_info)
                || control_info.Protect != PAGE_READWRITE) throw std::runtime_error("Controls are not RW");
            for (unsigned i = 0; i < 1000; ++i) {
                const std::uint32_t expected = (i & 1) ? 0 : 0x7fffffff;
                const bool changed = controls.publish(expected);
                if (!changed || controls.publish(expected)) throw std::runtime_error("Publication no-op detection failed");
                std::uint32_t actual = 0;
                SIZE_T count = 0;
                if (!ReadProcessMemory(child.hProcess, reinterpret_cast<LPCVOID>(retained_control), &actual, sizeof actual, &count)
                    || count != sizeof actual || actual != expected) throw std::runtime_error("Shared publication not visible");
            }
        }
        std::uint32_t retained = 1;
        SIZE_T count = 0;
        if (!ReadProcessMemory(child.hProcess, reinterpret_cast<LPCVOID>(retained_control), &retained, sizeof retained, &count)
            || count != sizeof retained || retained != 0) throw std::runtime_error("Control view did not survive local teardown");
        if (!reload.publish(0x7fffffff) || reload.publish(0x7fffffff) || !reload.publish(0))
            throw std::runtime_error("Runtime publication failed");
        for (unsigned i=0;i<1000;++i) {
            const auto mask = (i&1) ? 0u : 0x7fffffffu;
            if (!reload.publish(mask) || reload.publish(mask)) throw std::runtime_error("Runtime control no-op failed");
        }
        for (const auto& cave : reload.code_ranges()) {
            std::vector<std::uint8_t> bytes(cave.original.size());
            SIZE_T read=0;
            MEMORY_BASIC_INFORMATION info{};
            if (!ReadProcessMemory(child.hProcess, reinterpret_cast<LPCVOID>(cave.address), bytes.data(), bytes.size(), &read)
                || read!=bytes.size() || bytes!=cave.original
                || !VirtualQueryEx(child.hProcess, reinterpret_cast<LPCVOID>(cave.address), &info, sizeof info)
                || info.Protect!=PAGE_EXECUTE_READ) throw std::runtime_error("Publication changed executable code or protection");
        }
        if (reload.hook().address != target || reload.hook().replacement.front() != 0xe9)
            throw std::runtime_error("Hook address or encoding incorrect");
        const std::vector<metro::Patch> patches{{target, {0xb8, 1, 0, 0, 0, 0xc3}, {0xb8, 2, 0, 0, 0, 0xc3}}};
        metro::PatchTransaction transaction(patches);
        metro::ExecutableMemory memory(child.hProcess);
        bool applied = false;
        for (unsigned i = 0; i < 100 && !applied; ++i) {
            try {
                metro::FrozenThreads frozen(child.dwProcessId, patches);
                std::string error;
                if (!transaction.apply(memory, error)) throw std::runtime_error(error);
                MEMORY_BASIC_INFORMATION info{};
                if (!VirtualQueryEx(child.hProcess, reinterpret_cast<LPCVOID>(target), &info, sizeof info)
                    || info.Protect != PAGE_EXECUTE_READ) throw std::runtime_error("Protection was not restored");
                if (!transaction.restore(memory, error)) throw std::runtime_error(error);
                // Repeated master/feature transitions keep one thread freeze for
                // restore, replacement selection, and rollback to prior bytes.
                if (!transaction.apply(memory, error)) throw std::runtime_error(error);
                const std::vector<metro::Patch> alternate{{target, patches.front().original, {0xb8, 4, 0, 0, 0, 0xc3}}};
                for (unsigned toggle = 0; toggle < 25; ++toggle) {
                    if (!transaction.transition(memory, alternate, error) || !transaction.transition(memory, patches, error))
                        throw std::runtime_error("Repeated differential transition failed: " + error);
                }
                if (!memory.write(target, {0xb8, 3, 0, 0, 0, 0xc3})) throw std::runtime_error("Foreign setup failed");
                if (transaction.restore(memory, error)) throw std::runtime_error("Foreign bytes were overwritten");
                std::vector<std::uint8_t> foreign(6);
                if (!memory.read(target, foreign) || foreign[1] != 3) throw std::runtime_error("Foreign byte changed");
                // The dummy test owns the foreign write, so it can return it to an owned state.
                if (!memory.write(target, patches.front().replacement) || !transaction.restore(memory, error))
                    throw std::runtime_error("Dummy cleanup failed");
                applied = true;
            } catch (const std::exception&) { if (transaction.active() || i == 99) throw; Sleep(1); }
        }
        const auto previous = SuspendThread(child.hThread);
        if (previous != 0) throw std::runtime_error("Freeze leaked a thread suspend count");
        ResumeThread(child.hThread);
        std::cout << "Executable backend apply/restore, foreign preservation, protection and resume checks passed.\n";
        result = 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; }
    TerminateProcess(child.hProcess, 0);
    WaitForSingleObject(child.hProcess, 5000);
    CloseHandle(input); CloseHandle(child.hThread); CloseHandle(child.hProcess);
    return result;
}
