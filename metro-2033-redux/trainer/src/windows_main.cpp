#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <bcrypt.h>
#include <mmsystem.h>
#include <tlhelp32.h>
#include "metro/core.hpp"
#include "metro/build_profile.hpp"
#include "metro/windows_patch.hpp"
#include "metro/windows_no_reload.hpp"
#include "metro/controls.hpp"
#include "metro/windows_console.hpp"
#include <chrono>
#include <algorithm>
#include <array>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <cmath>
#include <memory>
#include <set>
#include <map>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {
namespace fs = std::filesystem;
volatile LONG stopping = 0;
BOOL WINAPI stop_handler(DWORD) { InterlockedExchange(&stopping, 1); return TRUE; }
struct Handle {
    HANDLE value = INVALID_HANDLE_VALUE;
    explicit Handle(HANDLE h) : value(h) {}
    ~Handle() { if (value != INVALID_HANDLE_VALUE && value != nullptr) CloseHandle(value); }
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
};
std::ofstream logfile;
bool plain_logging = true;
void log(const std::string& text) {
    SYSTEMTIME t{}; GetLocalTime(&t);
    std::ostringstream line;
    line << std::setfill('0') << std::setw(4) << t.wYear << '-' << std::setw(2)
         << t.wMonth << '-' << std::setw(2) << t.wDay << ' ' << std::setw(2)
         << t.wHour << ':' << std::setw(2) << t.wMinute << ':' << std::setw(2)
         << t.wSecond << " " << text;
    if (plain_logging) std::cout << line.str() << std::endl;
    if (logfile) { logfile << line.str() << '\n'; logfile.flush(); }
}
bool binding_down(unsigned binding) {
    if (!binding || !(GetAsyncKeyState(binding & 0xff) & 0x8000)) return false;
    const unsigned modifiers = ((GetAsyncKeyState(VK_CONTROL)&0x8000) ? 0x100 : 0)
        | ((GetAsyncKeyState(VK_MENU)&0x8000) ? 0x200 : 0)
        | ((GetAsyncKeyState(VK_SHIFT)&0x8000) ? 0x400 : 0);
    return (binding & 0x700) == modifiers;
}
// Quiet sine chime with a smooth envelope; no system alert or square-wave Beep.
std::vector<std::uint8_t> make_chime(bool enabled) {
    std::vector<std::uint8_t> sound;
    constexpr unsigned rate = 22050, count = rate / 6;
    sound.assign(44 + count * 2, 0);
    auto put = [&](unsigned offset, unsigned value, unsigned length) {
        for (unsigned i = 0; i < length; ++i) sound[offset + i] = static_cast<std::uint8_t>(value >> (8 * i));
    };
    memcpy(sound.data(), "RIFF", 4); put(4, sound.size() - 8, 4);
    memcpy(sound.data()+8, "WAVEfmt ", 8); put(16,16,4); put(20,1,2); put(22,1,2);
    put(24,rate,4); put(28,rate*2,4); put(32,2,2); put(34,16,2);
    memcpy(sound.data()+36,"data",4); put(40,count*2,4);
    for (unsigned i=0; i<count; ++i) {
        const double progress = static_cast<double>(i) / count;
        const double envelope = std::sin(3.141592653589793 * progress);
        const double frequency = enabled ? 660.0 : 440.0;
        const auto sample = static_cast<std::int16_t>(2300 * envelope * envelope * std::sin(6.283185307179586 * frequency * i / rate));
        put(44+i*2, static_cast<std::uint16_t>(sample), 2);
    }
    return sound;
}
const std::array<std::vector<std::uint8_t>, 2> chimes{{make_chime(false), make_chime(true)}};
void soft_sound(bool enabled) {
    PlaySoundW(nullptr, nullptr, 0);
    PlaySoundW(reinterpret_cast<LPCWSTR>(chimes[enabled ? 1 : 0].data()), nullptr,
        SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
}
void require(bool ok, const char* message) { if (!ok) throw std::runtime_error(message); }
std::vector<std::uint8_t> read_file(HANDLE file) {
    LARGE_INTEGER size{};
    require(GetFileSizeEx(file, &size) && size.QuadPart > 0 && size.QuadPart < 1024LL*1024*1024,
            "Cannot read game executable size.");
    std::vector<std::uint8_t> data(static_cast<std::size_t>(size.QuadPart));
    DWORD read = 0;
    require(ReadFile(file, data.data(), static_cast<DWORD>(data.size()), &read, nullptr)
            && read == data.size(), "Cannot read game executable.");
    return data;
}
std::string sha256(const std::vector<std::uint8_t>& bytes) {
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    BCRYPT_HASH_HANDLE hash = nullptr;
    require(BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) >= 0,
            "Cannot initialize SHA-256.");
    auto status = BCryptCreateHash(algorithm, &hash, nullptr, 0, nullptr, 0, 0);
    if (status >= 0) status = BCryptHashData(hash, const_cast<PUCHAR>(bytes.data()), static_cast<ULONG>(bytes.size()), 0);
    std::array<UCHAR, 32> digest{};
    if (status >= 0) status = BCryptFinishHash(hash, digest.data(), static_cast<ULONG>(digest.size()), 0);
    if (hash) BCryptDestroyHash(hash);
    BCryptCloseAlgorithmProvider(algorithm, 0);
    require(status >= 0, "SHA-256 calculation failed.");
    std::ostringstream out;
    for (auto byte : digest) out << std::hex << std::setfill('0') << std::setw(2) << static_cast<unsigned>(byte);
    return out.str();
}
template<class T> T at(const std::vector<std::uint8_t>& data, std::size_t offset) {
    require(offset <= data.size() && sizeof(T) <= data.size() - offset, "Truncated PE image.");
    T result{}; memcpy(&result, data.data() + offset, sizeof result); return result;
}
struct Image {
    IMAGE_NT_HEADERS64 nt;
    std::size_t nt_offset;
    std::vector<IMAGE_SECTION_HEADER> sections;
};
Image parse_image(const std::vector<std::uint8_t>& bytes) {
    const auto dos = at<IMAGE_DOS_HEADER>(bytes, 0);
    require(dos.e_magic == IMAGE_DOS_SIGNATURE && dos.e_lfanew > 0, "Invalid DOS header.");
    Image result{}; result.nt_offset = static_cast<std::size_t>(dos.e_lfanew);
    result.nt = at<IMAGE_NT_HEADERS64>(bytes, result.nt_offset);
    require(result.nt.Signature == IMAGE_NT_SIGNATURE && result.nt.FileHeader.Machine == IMAGE_FILE_MACHINE_AMD64
        && result.nt.OptionalHeader.Magic == IMAGE_NT_OPTIONAL_HDR64_MAGIC, "Expected Windows x64 executable.");
    const auto section_start = result.nt_offset + sizeof(DWORD) + sizeof(IMAGE_FILE_HEADER)
        + result.nt.FileHeader.SizeOfOptionalHeader;
    for (unsigned i = 0; i < result.nt.FileHeader.NumberOfSections; ++i)
        result.sections.push_back(at<IMAGE_SECTION_HEADER>(bytes, section_start + i * sizeof(IMAGE_SECTION_HEADER)));
    return result;
}
std::vector<DWORD> find_processes() {
    Handle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
    require(snapshot.value != INVALID_HANDLE_VALUE, "Cannot enumerate processes.");
    PROCESSENTRY32W entry{}; entry.dwSize = sizeof entry;
    std::vector<DWORD> found;
    if (Process32FirstW(snapshot.value, &entry)) do {
        if (_wcsicmp(entry.szExeFile, L"metro.exe") == 0) found.push_back(entry.th32ProcessID);
    } while (Process32NextW(snapshot.value, &entry));
    return found;
}
class ProcessMemory final : public metro::Memory {
public:
    explicit ProcessMemory(HANDLE process) : process_(process) {}
    bool read(std::uintptr_t address, std::vector<std::uint8_t>& out) override {
        SIZE_T done = 0;
        return ReadProcessMemory(process_, reinterpret_cast<LPCVOID>(address), out.data(), out.size(), &done)
            && done == out.size();
    }
    bool write(std::uintptr_t address, const std::vector<std::uint8_t>& bytes) override {
        MEMORY_BASIC_INFORMATION info{};
        if (!VirtualQueryEx(process_, reinterpret_cast<LPCVOID>(address), &info, sizeof info)) return false;
        // This backend intentionally never changes page protection or writes executable pages.
        const auto protection = info.Protect & 0xff;
        if (info.State != MEM_COMMIT || (info.Protect & (PAGE_GUARD | PAGE_NOACCESS)) || (protection != PAGE_READWRITE && protection != PAGE_WRITECOPY)
            || bytes.empty() || address < reinterpret_cast<std::uintptr_t>(info.BaseAddress)
            || bytes.size() > info.RegionSize - (address - reinterpret_cast<std::uintptr_t>(info.BaseAddress))) return false;
        SIZE_T done = 0;
        return WriteProcessMemory(process_, reinterpret_cast<LPVOID>(address), bytes.data(), bytes.size(), &done)
            && done == bytes.size();
    }
private:
    HANDLE process_;
};
std::wstring process_path(HANDLE process) {
    std::wstring path(32768, L'\0'); DWORD length = static_cast<DWORD>(path.size());
    require(QueryFullProcessImageNameW(process, 0, path.data(), &length), "Cannot query game path.");
    path.resize(length);
    return path;
}
std::uintptr_t validate_process(DWORD pid, HANDLE process, const std::wstring& path, const Image& expected,
                                const std::vector<std::uint8_t>& file) {
    Handle modules(CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid));
    require(modules.value != INVALID_HANDLE_VALUE, "Cannot enumerate game modules.");
    MODULEENTRY32W module{}; module.dwSize = sizeof module;
    require(Module32FirstW(modules.value, &module), "Cannot locate main image.");
    require(_wcsicmp(module.szExePath, path.c_str()) == 0, "Main module path mismatch.");
    require(module.modBaseSize == expected.nt.OptionalHeader.SizeOfImage, "Loaded image size mismatch.");
    const auto base = reinterpret_cast<std::uintptr_t>(module.modBaseAddr);
    ProcessMemory memory(process);
    std::vector<std::uint8_t> headers(expected.nt.OptionalHeader.SizeOfHeaders);
    require(memory.read(base, headers), "Cannot read loaded image headers.");
    auto loaded = parse_image(headers);
    // Runtime capture confirms the loader rewrites only ImageBase in the NT header.
    require(loaded.nt.OptionalHeader.ImageBase == base, "Loaded header base mismatch.");
    loaded.nt.OptionalHeader.ImageBase = expected.nt.OptionalHeader.ImageBase;
    require(memcmp(&loaded.nt, &expected.nt, sizeof loaded.nt) == 0, "Loaded image identity mismatch.");
    require(!metro::expected_runtime_text_sha256.empty(),
            "No verified unpacked runtime text profile is available for this build.");
    const auto text = std::find_if(expected.sections.begin(), expected.sections.end(),
        [](const IMAGE_SECTION_HEADER& section) { return memcmp(section.Name, ".text", 5) == 0; });
    require(text != expected.sections.end(), "Missing .text section.");
    std::vector<std::uint8_t> content(text->Misc.VirtualSize);
    require(!content.empty() && memory.read(base + text->VirtualAddress, content), "Cannot read runtime .text.");
    // Normalize PE base relocations to the preferred image base before hashing.
    const auto delta = static_cast<std::uint64_t>(base) - expected.nt.OptionalHeader.ImageBase;
    const auto reloc = expected.nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_BASERELOC];
    auto file_offset = [&](std::uint32_t rva, std::size_t length) -> std::size_t {
        for (const auto& section : expected.sections) {
            if (rva < section.VirtualAddress) continue;
            const auto offset = static_cast<std::size_t>(rva - section.VirtualAddress);
            if (offset <= section.SizeOfRawData && length <= section.SizeOfRawData - offset)
                return static_cast<std::size_t>(section.PointerToRawData) + offset;
        }
        throw std::runtime_error("Invalid relocation directory.");
    };
    std::size_t consumed = 0;
    while (consumed < reloc.Size) {
        require(reloc.Size - consumed >= sizeof(IMAGE_BASE_RELOCATION), "Truncated relocation block.");
        require(consumed <= UINT32_MAX - reloc.VirtualAddress, "Relocation address overflow.");
        const auto offset = file_offset(reloc.VirtualAddress + static_cast<std::uint32_t>(consumed),
                                        sizeof(IMAGE_BASE_RELOCATION));
        const auto block = at<IMAGE_BASE_RELOCATION>(file, offset);
        require(block.SizeOfBlock >= sizeof block && block.SizeOfBlock <= reloc.Size - consumed
                && (block.SizeOfBlock - sizeof block) % sizeof(WORD) == 0, "Invalid relocation block.");
        const auto full_offset = file_offset(reloc.VirtualAddress + static_cast<std::uint32_t>(consumed), block.SizeOfBlock);
        for (std::size_t entry = sizeof block; entry < block.SizeOfBlock; entry += sizeof(WORD)) {
            const auto value = at<WORD>(file, full_offset + entry);
            const auto type = value >> 12;
            const auto rva = static_cast<std::uint64_t>(block.VirtualAddress) + (value & 0xfff);
            if (type == IMAGE_REL_BASED_ABSOLUTE || rva < text->VirtualAddress
                || rva >= static_cast<std::uint64_t>(text->VirtualAddress) + content.size()) continue;
            require(type == IMAGE_REL_BASED_DIR64, "Unsupported text relocation type.");
            const auto position = static_cast<std::size_t>(rva - text->VirtualAddress);
            auto pointer = at<std::uint64_t>(content, position);
            pointer -= delta;
            memcpy(content.data() + position, &pointer, sizeof pointer);
        }
        consumed += block.SizeOfBlock;
    }
    require(sha256(content) == metro::expected_runtime_text_sha256,
            "Runtime .text does not match the verified unpacked build.");
    return base;
}
}
int wmain(int argc, wchar_t** argv) {
    metro::ui::Model model;
    std::unique_ptr<metro::ui::WindowsConsole> console;
    const char* console_error = nullptr;
    auto refresh = [&](bool poll_input = true) {
        if (!console) return;
        if (poll_input) console->refresh();
        if (const auto error = console->error(); error && error != console_error) {
            console_error = error;
            log(std::string("UI: ") + error);
        }
    };
    auto show = [&](metro::ui::Lifecycle lifecycle, const char* status) noexcept {
        try {
            model.lifecycle = lifecycle;
            model.status = status;
            if (console) console->update(model);
            refresh(false);
        } catch (...) { /* Presentation must never interrupt game cleanup. */ }
    };
    try {
        bool check = false;
        bool stop = argc == 2 && std::wstring(argv[1]) == L"--stop";
        if (stop) {
            const auto pids = find_processes();
            require(pids.size() == 1, "--stop requires exactly one running metro.exe.");
            const auto name = L"Local\\MetroReduxTrainerStop-" + std::to_wstring(pids.front());
            Handle event(OpenEventW(EVENT_MODIFY_STATE, FALSE, name.c_str()));
            require(event.value != nullptr && SetEvent(event.value), "No active trainer stop event is available.");
            std::cout << "Graceful trainer stop requested. Check the trainer log for restoration status." << std::endl;
            return 0;
        }
        if (argc == 2 && std::wstring(argv[1]) == L"--check") check = true;
        else require(argc == 1, "Usage: metro-trainer.exe [--check|--stop]");
        std::wstring own(32768, L'\0');
        auto size = GetModuleFileNameW(nullptr, own.data(), static_cast<DWORD>(own.size()));
        require(size > 0 && size < own.size(), "Cannot locate trainer directory."); own.resize(size);
        const auto directory = fs::path(own).parent_path();
        model.logfile = (directory / L"metro-trainer.log").u8string();
        logfile.open(directory / L"metro-trainer.log", std::ios::app);
        require(static_cast<bool>(logfile), "Cannot create trainer log next to the trainer.");
        std::ifstream settings(directory / L"metro-trainer.ini");
        require(static_cast<bool>(settings), "Missing metro-trainer.ini next to trainer.");
        std::ostringstream config_text; config_text << settings.rdbuf();
        const auto config = metro::parse_config(config_text.str());
        SetConsoleCtrlHandler(stop_handler, TRUE);
        if (!check) {
            model.config = config;
            for (const char* key : {"unlimited_ammo", "unlimited_throwables", "unlimited_medkits", "unlimited_filters",
                                   "indestructible_mask", "no_reload", "god_mode", "unlimited_flashlight"})
                model.selected[key] = config.feature_enabled(key);
            for (const auto& entry : metro::verified_no_reload_classes())
                model.selected[entry.feature] = config.feature_enabled(entry.feature);
            plain_logging = false;
            console = std::make_unique<metro::ui::WindowsConsole>();
            show(metro::ui::Lifecycle::Waiting, "Waiting for Metro 2033 Redux. Ctrl+C exits.");
        }
        log(check ? "Read-only check: verifying the running game." : "Waiting for Metro 2033 Redux. Ctrl+C exits.");
        do {
            const auto pids = find_processes();
            require(pids.size() <= 1, "Multiple metro.exe processes found; close duplicates.");
            if (pids.empty()) {
                require(!check, "Game is not running; start it and run --check again.");
                refresh(); Sleep(250); continue;
            }
            Handle process(OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE |
                (check ? 0 : PROCESS_VM_WRITE | PROCESS_VM_OPERATION), FALSE, pids.front()));
            require(process.value != nullptr, "Cannot open game process with required access.");
            if (!check) show(metro::ui::Lifecycle::Preparing, "Validating game identity and hook profiles.");
            // The open handle pins this process, so its path and the file read below belong to it.
            const auto path = process_path(process.value);
            log("Game: " + fs::path(path).u8string());
            Handle disk(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                    nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
            require(disk.value != INVALID_HANDLE_VALUE, "Cannot open the game executable.");
            const auto bytes = read_file(disk.value);
            const auto digest = sha256(bytes);
            log("Game SHA-256: " + digest);
            require(digest == metro::supported_sha256, "Unsupported executable build; no memory was changed.");
            // The file only selects the expected headers; the runtime .text hash is a compiled-in constant.
            const auto image = parse_image(bytes);
            std::uintptr_t base = 0;
            const auto startup_deadline = GetTickCount64() + 60000;
            bool announced = false;
            for (;;) {
                try { base = validate_process(pids.front(), process.value, path, image, bytes); break; }
                catch (const std::exception&) {
                    if (check || GetTickCount64() >= startup_deadline) throw;
                    require(WaitForSingleObject(process.value, 0) != WAIT_OBJECT_0,
                            "Game exited while waiting for unpacking.");
                    if (!announced) { log("Waiting up to 60 seconds for the verified unpacked game image."); announced = true; }
                    if (InterlockedCompareExchange(&stopping, 0, 0)) {
                        show(metro::ui::Lifecycle::Stopped, "Stopped. No memory was changed.");
                        log("Stopped. No memory was changed."); return 0;
                    }
                    refresh(); Sleep(250);
                }
            }
            (void)base;
            log("Loaded game identity verified.");
            std::vector<metro::Patch> patches;
            std::map<std::string, bool> selected;
            std::map<std::string, bool> feature_patches;
            const auto text_section = std::find_if(image.sections.begin(), image.sections.end(),
                [](const IMAGE_SECTION_HEADER& section) { return memcmp(section.Name, ".text", 5) == 0; });
            require(text_section != image.sections.end(), "No text section for patch validation.");
            std::vector<std::uint8_t> runtime_text(text_section->Misc.VirtualSize);
            ProcessMemory readonly(process.value);
            require(readonly.read(base + text_section->VirtualAddress, runtime_text), "Cannot read patch context.");
            for (const auto& definition : metro::verified_conditional_sites()) {
                require(definition.rva < image.nt.OptionalHeader.SizeOfImage
                    && definition.original.size() <= image.nt.OptionalHeader.SizeOfImage - definition.rva,
                    "Patch profile exceeds image bounds.");
                const auto context = metro::parse_pattern(definition.context);
                require(context.size() >= 16, "Patch context is too short.");
                const auto matches = metro::scan_pattern(runtime_text, context);
                require(matches.size() == 1 && matches.front() + text_section->VirtualAddress == definition.context_rva,
                    "Patch context is not unique at the reviewed location.");
                std::vector<std::uint8_t> original(definition.original.size());
                require(readonly.read(base + definition.rva, original) && original == definition.original,
                    "Original patch bytes do not match the verified profile.");
                feature_patches[definition.feature] = true;
                selected[definition.feature] = config.feature_enabled(definition.feature);
            }
            for (const auto& site : metro::verified_no_reload_sites()) {
                require(site.rva < image.nt.OptionalHeader.SizeOfImage
                    && site.original.size() <= image.nt.OptionalHeader.SizeOfImage-site.rva,
                    "No-reload hook exceeds image bounds.");
                const auto reload_context = metro::parse_pattern(site.context);
                require(reload_context.size() >= 16, "No-reload context too short.");
                const auto reload_matches = metro::scan_pattern(runtime_text, reload_context);
                require(reload_matches.size() == 1 && reload_matches.front() + text_section->VirtualAddress == site.rva,
                        "No-reload context does not match its unique reviewed location.");
                std::vector<std::uint8_t> original(site.original.size());
                require(readonly.read(base+site.rva, original) && original == site.original,
                        "No-reload original bytes differ from reviewed profile.");
            }
            selected["no_reload"] = config.no_reload;
            for (const auto& entry : metro::verified_no_reload_classes()) selected[entry.feature] = config.feature_enabled(entry.feature);
            if (check) { log("All available patch profile contexts and original bytes verified read-only."); return 0; }
            for (const char* name : {"unlimited_ammo", "unlimited_throwables", "unlimited_medkits", "unlimited_filters", "indestructible_mask", "no_reload", "god_mode", "unlimited_flashlight"})
                require(!config.feature_enabled(name) || std::string(name) == "no_reload" || feature_patches.count(name), "Requested feature has no verified patch profile.");
            for (const auto& feature : config.extra_features)
                require(!feature.second || selected.count(feature.first) || feature_patches.count(feature.first), "Requested feature has no verified patch profile.");
            const auto mutex_name = L"Local\\MetroReduxTrainer-" + std::to_wstring(pids.front());
            Handle ownership(CreateMutexW(nullptr, TRUE, mutex_name.c_str()));
            require(ownership.value != nullptr && GetLastError() != ERROR_ALREADY_EXISTS,
                    "Another trainer instance already owns this game process.");
            const auto event_name = L"Local\\MetroReduxTrainerStop-" + std::to_wstring(pids.front());
            Handle stop_event(CreateEventW(nullptr, TRUE, FALSE, event_name.c_str()));
            require(stop_event.value != nullptr && GetLastError() != ERROR_ALREADY_EXISTS,
                    "Cannot create exclusive trainer stop event.");
            show(metro::ui::Lifecycle::Preparing, "Preparing disabled hooks and shared controls.");
            metro::NoReloadRuntime reload(process.value, base, metro::verified_no_reload_classes());
            const auto reload_hooks = reload.hooks();
            const auto reload_ranges = reload.code_ranges();
            patches.insert(patches.end(), reload_hooks.begin(), reload_hooks.end());
            patches.insert(patches.end(), reload_ranges.begin(), reload_ranges.end());
            auto transaction = std::make_unique<metro::PatchTransaction>(std::vector<metro::Patch>{});
            bool master = config.enabled;
            metro::ExecutableMemory writable(process.value);
            auto change = [&](bool restoring) {
                for (unsigned attempt = 0; attempt < 100; ++attempt) {
                    try {
                        const auto acquisition_started = std::chrono::steady_clock::now();
                        std::unique_ptr<metro::FrozenThreads> frozen;
                        frozen = std::make_unique<metro::FrozenThreads>(pids.front(), patches);
                        const auto freeze_started = std::chrono::steady_clock::now();
                        std::string error;
                        bool ok = false;
                        try { ok = restoring ? transaction->restore(writable, error) : transaction->apply(writable, error); }
                        catch (const std::exception&) { /* Recovery below must stay inside the freeze. */ }
                        if (!ok) {
                            // Retry rollback briefly while frozen, then preserve foreign bytes and report failure.
                            for (unsigned recovery = 0; recovery < 10; ++recovery) {
                                if (WaitForSingleObject(process.value, 0) == WAIT_OBJECT_0) return false;
                                try {
                                    std::string rollback;
                                    if (transaction->restore(writable, rollback) && writable.restore_protections()) break;
                                } catch (const std::exception&) { }
                                Sleep(10);
                            }
                        }
                        const bool protections_ok = writable.restore_protections();
                        frozen.reset();
                        const auto freeze_finished = std::chrono::steady_clock::now();
                        log(std::string(restoring ? "Restoration" : "Installation") + " thread acquisition: " +
                            std::to_string(std::chrono::duration_cast<std::chrono::microseconds>(
                                freeze_started - acquisition_started).count()) + " us; suspended work and resume: " +
                            std::to_string(std::chrono::duration_cast<std::chrono::microseconds>(
                                freeze_finished - freeze_started).count()) + " us (excludes thread acquisition)");
                        if (!ok || !protections_ok) {
                            log(transaction->active() || !protections_ok
                                ? "FATAL: restoration incomplete; foreign or inaccessible bytes preserved. Resuming game threads."
                                : "Patch operation failed; owned bytes and protections restored.");
                        }
                        return ok && protections_ok;
                    } catch (const std::exception& error) {
                        if (WaitForSingleObject(process.value, 0) == WAIT_OBJECT_0) return false;
                        if (attempt == 99) throw;
                        if (attempt == 0) log(std::string("Waiting for a safe thread position: ") + error.what());
                        Sleep(10);
                    }
                }
                return false;
            };
            auto sync = [&]() {
                metro::ui::acknowledge(model, master, selected, reload.current_controls());
                show(metro::ui::Lifecycle::Attached, master ? "Selected features active." : "Paused; selections retained.");
            };
            bool restoration_failed = false;
            try {
                show(metro::ui::Lifecycle::Installing, "Installing hooks with controls disabled.");
                transaction = std::make_unique<metro::PatchTransaction>(reload_hooks);
                require(change(false), "Could not apply selected features; inspect recovery log.");
                reload.publish(metro::effective_controls(master, selected));
                sync();
                auto log_selection = [&]() {
                    std::string snapshot = "Selection snapshot: enabled=" + std::string(master ? "true" : "false");
                    for (const auto& feature : selected) snapshot += ";" + feature.first + "=" + (feature.second ? "true" : "false");
                    log(snapshot);
                };
                log_selection();
                std::map<std::string, metro::KeyLatch> latches;
                for (const auto& binding : config.hotkeys)
                    latches[binding.first].update(binding_down(binding.second));
                log("Hotkeys ready. " + metro::ui::binding_label(config.hotkeys.at("enabled")) + " toggles master; Ctrl+C restores original bytes.");
                for (const auto& binding : config.hotkeys)
                    log("Binding " + binding.first + " = " + metro::ui::binding_label(binding.second));
                while (!InterlockedCompareExchange(&stopping, 0, 0)
                       && WaitForSingleObject(stop_event.value, 0) != WAIT_OBJECT_0) {
                    if (WaitForSingleObject(process.value, 20) == WAIT_OBJECT_0) {
                        model.master = false; model.effective.clear();
                        show(metro::ui::Lifecycle::Stopped, "Game exited. Trainer finished.");
                        log("Game exited. Trainer finished."); return 0;
                    }
                    refresh();
                    // Snapshot all eligible bindings before applying toggles.
                    // Presentation updates never consume console input.
                    std::vector<std::string> pressed;
                    DWORD foreground_pid = 0;
                    GetWindowThreadProcessId(GetForegroundWindow(), &foreground_pid);
                    for (const auto& binding : config.hotkeys) {
                        const bool down = binding_down(binding.second);
                        const bool rising = latches[binding.first].update(down);
                        if (console->hotkey_pressed(binding.second, down, rising, foreground_pid == pids.front()))
                            pressed.push_back(binding.first);
                    }
                    for (const auto& name : pressed) {
                        const auto binding = *config.hotkeys.find(name);
                        if (binding.first != "enabled" && !selected.count(binding.first)) { log("No verified profile for " + binding.first); continue; }
                        const auto previous_mask = metro::effective_controls(master, selected);
                        if (binding.first == "enabled") master = !master;
                        else selected[binding.first] = !selected[binding.first];
                        const auto next_mask = metro::effective_controls(master, selected);
                        const auto publication_started = std::chrono::steady_clock::now();
                        const bool published = reload.publish(next_mask);
                        const auto publication_ns = std::chrono::duration_cast<std::chrono::nanoseconds>(
                            std::chrono::steady_clock::now() - publication_started).count();
                        if (published) log("Control publication: " + std::to_string(publication_ns) + " ns (local store, no freeze)");
                        const bool on = binding.first == "enabled" ? master : selected[binding.first];
                        log(binding.first + (on ? " enabled" : " disabled") + (!master && binding.first != "enabled" ? " (trainer paused)" : ""));
                        log_selection();
                        sync();
                        if (config.sounds && previous_mask != next_mask) soft_sound(on);
                    }
                }
                reload.publish(0);
                model.master = false; model.effective.clear();
                show(metro::ui::Lifecycle::Restoring, "Restoring original instructions; keep this window open.");
                if (WaitForSingleObject(process.value, 0) != WAIT_OBJECT_0) {
                    restoration_failed = !change(true);
                    require(!restoration_failed, "Could not restore all patches; foreign or inaccessible bytes were preserved.");
                }
            } catch (...) {
                reload.publish(0);
                model.master = false; model.effective.clear();
                show(metro::ui::Lifecycle::Restoring, "Recovering original instructions; keep this window open.");
                if (!restoration_failed && WaitForSingleObject(process.value, 0) != WAIT_OBJECT_0) {
                    try { restoration_failed = !change(true); }
                    catch (...) { restoration_failed = true; }
                }
                if (restoration_failed) {
                    model.error = "Restoration incomplete. Close the game before restarting trainer.";
                    log("FATAL: " + model.error);
                }
                throw;
            }
            show(metro::ui::Lifecycle::Stopped, "Original bytes restored. Trainer finished.");
            log("Original bytes restored. Trainer finished."); return 0;
        } while (!InterlockedCompareExchange(&stopping, 0, 0));
        show(metro::ui::Lifecycle::Stopped, "Stopped. No memory was changed.");
        log("Stopped. No memory was changed."); return 0;
    } catch (const std::exception& error) {
        log(std::string("ERROR: ") + error.what());
        if (model.error.empty()) model.error = error.what();
        show(metro::ui::Lifecycle::Error, "Trainer stopped. Inspect the log; resolve the error before restarting.");
        return 1;
    }
}
