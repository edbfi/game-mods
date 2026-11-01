// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <windows.h>
#include <cstdint>
#include <cstring>
#include <stdexcept>

namespace metro {
// Coherent pagefile-backed views, local atomic publication, remote aligned DWORD load.
// The remote view survives destruction: displaced calls may return to retained caves.
class SharedControls {
    HANDLE mapping_ = nullptr;
    volatile LONG* local_ = nullptr;
    std::uintptr_t remote_ = 0;
    std::uint32_t current_ = 0;
public:
    explicit SharedControls(HANDLE process) {
        static_assert(sizeof(LONG) == sizeof(std::uint32_t));
        using MapRemote = PVOID (WINAPI*)(HANDLE, HANDLE, ULONG64, PVOID, SIZE_T, ULONG, ULONG, ULONG);
        // MapViewOfFile2 is inline, not exported. Resolve its documented backing API.
        const auto module = GetModuleHandleW(L"kernelbase.dll");
        auto proc = module ? GetProcAddress(module, "MapViewOfFileNuma2") : nullptr;
        if (!proc) {
            const auto kernel = GetModuleHandleW(L"kernel32.dll");
            proc = kernel ? GetProcAddress(kernel, "MapViewOfFileNuma2") : nullptr;
        }
        MapRemote map_remote = nullptr;
        static_assert(sizeof map_remote == sizeof proc);
        std::memcpy(&map_remote, &proc, sizeof proc);
        if (!map_remote) throw std::runtime_error("Shared controls require Windows 10 version 1703 or newer");
        mapping_ = CreateFileMappingW(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0, 4096, nullptr);
        if (!mapping_) throw std::runtime_error("Cannot create shared controls");
        local_ = static_cast<volatile LONG*>(MapViewOfFile(mapping_, FILE_MAP_WRITE | FILE_MAP_READ, 0, 0, 4096));
        if (local_) {
            InterlockedExchange(local_, 0);
            remote_ = reinterpret_cast<std::uintptr_t>(map_remote(mapping_, process, 0, nullptr, 4096, 0, PAGE_READWRITE, 0xffffffff));
        }
        if (!local_ || !remote_) {
            if (local_) UnmapViewOfFile(const_cast<LONG*>(local_));
            CloseHandle(mapping_);
            throw std::runtime_error("Cannot map coherent shared controls into game");
        }
    }
    ~SharedControls() {
        InterlockedExchange(local_, 0);
        UnmapViewOfFile(const_cast<LONG*>(local_));
        CloseHandle(mapping_);
    }
    SharedControls(const SharedControls&) = delete;
    SharedControls& operator=(const SharedControls&) = delete;
    std::uintptr_t remote_address() const { return remote_; }
    bool publish(std::uint32_t mask) {
        if (mask == current_) return false;
        InterlockedExchange(local_, static_cast<LONG>(mask));
        current_ = mask;
        return true;
    }
    std::uint32_t current() const { return current_; }
};
}
