#pragma once
#include "metro/core.hpp"
#include <windows.h>
#include <tlhelp32.h>
#include <algorithm>
#include <memory>
#include <stdexcept>
#include <vector>

namespace metro {
// Suspend counts are balanced individually; preexisting suspensions are preserved.
class FrozenThreads {
    struct Thread {
        HANDLE handle;
        explicit Thread(HANDLE h) : handle(h) {}
        bool suspended = false;
        ~Thread() { if (suspended) ResumeThread(handle); CloseHandle(handle); }
    };
    std::vector<std::unique_ptr<Thread>> threads_;
public:
    FrozenThreads(DWORD pid, const std::vector<Patch>& patches) {
        std::vector<DWORD> known;
        unsigned stable = 0;
        for (unsigned pass = 0; pass < 16 && stable < 2; ++pass) {
            HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if (snapshot == INVALID_HANDLE_VALUE) throw std::runtime_error("Cannot enumerate game threads.");
            THREADENTRY32 entry{}; entry.dwSize = sizeof entry;
            std::vector<DWORD> ids;
            if (Thread32First(snapshot, &entry)) do {
                if (entry.th32OwnerProcessID == pid) ids.push_back(entry.th32ThreadID);
            } while (Thread32Next(snapshot, &entry));
            const auto last_error = GetLastError();
            CloseHandle(snapshot);
            if (last_error != ERROR_NO_MORE_FILES) throw std::runtime_error("Incomplete game thread enumeration.");
            std::sort(ids.begin(), ids.end());
            if (ids.empty()) throw std::runtime_error("No live game threads.");
            for (const auto id : ids) {
                if (std::binary_search(known.begin(), known.end(), id)) continue;
                HANDLE handle = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, FALSE, id);
                if (!handle) throw std::runtime_error("Cannot open all game threads.");
                auto thread = std::make_unique<Thread>(handle);
                if (GetProcessIdOfThread(handle) != pid) throw std::runtime_error("Thread owner changed during enumeration.");
                if (SuspendThread(handle) == static_cast<DWORD>(-1)) throw std::runtime_error("Cannot suspend game thread.");
                thread->suspended = true;
                threads_.push_back(std::move(thread));
            }
            stable = ids == known ? stable + 1 : 0;
            known = std::move(ids);
        }
        if (stable < 2) throw std::runtime_error("Game thread enumeration did not stabilize.");
        for (const auto& thread : threads_) {
            CONTEXT context{}; context.ContextFlags = CONTEXT_CONTROL;
            if (!GetThreadContext(thread->handle, &context)) throw std::runtime_error("Cannot inspect suspended game thread.");
            for (const auto& patch : patches)
                if (context.Rip >= patch.address && context.Rip - patch.address < patch.original.size())
                    throw std::runtime_error("A game thread is executing a patch range; retry required.");
        }
    }
    FrozenThreads(const FrozenThreads&) = delete;
    FrozenThreads& operator=(const FrozenThreads&) = delete;
};
// Use only while FrozenThreads is alive. Every write restores protection before returning.
class ExecutableMemory final : public Memory {
    HANDLE process_;
    struct Protection { void* address; SIZE_T length; DWORD original; };
    std::vector<Protection> pending_;
public:
    explicit ExecutableMemory(HANDLE process) : process_(process) {}
    bool read(std::uintptr_t address, std::vector<std::uint8_t>& out) override {
        SIZE_T done = 0;
        return ReadProcessMemory(process_, reinterpret_cast<LPCVOID>(address), out.data(), out.size(), &done) && done == out.size();
    }
    bool restore_protections() {
        for (std::size_t i = pending_.size(); i > 0; --i) {
            const auto& protection = pending_[i - 1];
            DWORD ignored = 0;
            if (VirtualProtectEx(process_, protection.address, protection.length, protection.original, &ignored))
                pending_.erase(pending_.begin() + static_cast<std::ptrdiff_t>(i - 1));
        }
        return pending_.empty();
    }
    bool write(std::uintptr_t address, const std::vector<std::uint8_t>& bytes) override {
        if (bytes.empty() || !restore_protections()) return false;
        pending_.reserve(bytes.size() / 4096 + 2);
        auto cursor = address;
        const auto end = address + bytes.size();
        if (end < address) return false;
        while (cursor < end) {
            MEMORY_BASIC_INFORMATION region{};
            if (!VirtualQueryEx(process_, reinterpret_cast<LPCVOID>(cursor), &region, sizeof region)
                || region.State != MEM_COMMIT || (region.Protect & (PAGE_GUARD | PAGE_NOACCESS))) {
                restore_protections(); return false;
            }
            const auto region_base = reinterpret_cast<std::uintptr_t>(region.BaseAddress);
            if (region.RegionSize > UINTPTR_MAX - region_base || region_base + region.RegionSize <= cursor) {
                restore_protections(); return false;
            }
            const auto length = std::min(end - cursor, region_base + region.RegionSize - cursor);
            DWORD old = 0;
            if (!VirtualProtectEx(process_, reinterpret_cast<LPVOID>(cursor), length, PAGE_EXECUTE_READWRITE, &old)) {
                restore_protections(); return false;
            }
            pending_.push_back({reinterpret_cast<LPVOID>(cursor), length, old});
            cursor += length;
        }
        SIZE_T done = 0;
        const bool written = WriteProcessMemory(process_, reinterpret_cast<LPVOID>(address), bytes.data(), bytes.size(), &done)
            && done == bytes.size();
        const bool flushed = FlushInstructionCache(process_, reinterpret_cast<LPCVOID>(address), bytes.size()) != FALSE;
        const bool protected_again = restore_protections();
        return written && flushed && protected_again;
    }
};
}
