#pragma once
#include "metro/no_reload.hpp"
#include "metro/conditional_hooks.hpp"
#include "metro/pressure.hpp"
#include "metro/windows_controls.hpp"
#include <windows.h>
#include <algorithm>
#include <stdexcept>

namespace metro {
// The cave and selector pages intentionally survive detach: a resumed thread may still hold
// a return address inside the cave. Windows reclaims them when the game exits.
class NoReloadRuntime {
    HANDLE process_;
    std::uintptr_t allocation_ = 0;
    SharedControls controls_;
    std::vector<NoReloadClass> classes_;
    std::vector<NoReloadImage> images_;
public:
    NoReloadRuntime(HANDLE process, std::uintptr_t base, std::vector<NoReloadClass> classes)
        : process_(process), controls_(process), classes_(std::move(classes)) {
        if (classes_.empty() || classes_.size() != no_reload_class_count) throw std::runtime_error("Invalid no-reload class registry");
        // One executable page per hook site; controls are a separate shared RW view.
        const auto sites = verified_no_reload_sites();
        if (sites.empty()) throw std::runtime_error("Unexpected no-reload hook count");
        const auto conditional = verified_conditional_sites();
        const auto pressure = verified_pressure_sites();
        const auto allocation_size = 4096 * (sites.size() + conditional.size() + pressure.size());
        SYSTEM_INFO info{}; GetSystemInfo(&info);
        const auto granularity = static_cast<std::uintptr_t>(info.dwAllocationGranularity);
        const auto origin = (base + no_reload_hook_rva) & ~(granularity - 1);
        for (std::uintptr_t delta = granularity; delta < 0x7fff0000 && !allocation_; delta += granularity) {
            for (unsigned direction = 0; direction < 2 && !allocation_; ++direction) {
                if ((!direction && origin > UINTPTR_MAX-delta) || (direction && origin < delta)) continue;
                const auto candidate = direction ? origin-delta : origin+delta;
                allocation_ = reinterpret_cast<std::uintptr_t>(VirtualAllocEx(process_, reinterpret_cast<void*>(candidate), allocation_size, MEM_RESERVE|MEM_COMMIT, PAGE_READWRITE));
            }
        }
        if (!allocation_) throw std::runtime_error("Cannot allocate nearby no-reload cave");
        try {
            for (std::size_t i=0; i<sites.size()+conditional.size()+pressure.size(); ++i) {
                const auto address = allocation_+4096*i;
                auto image = i < sites.size()
                    ? make_no_reload_image(base, address, controls_.remote_address(), classes_, sites[i])
                    : i < sites.size()+conditional.size()
                    ? make_conditional_image(base, address, controls_.remote_address(), conditional[i-sites.size()])
                    : make_pressure_image(base, address, controls_.remote_address(), pressure[i-sites.size()-conditional.size()].site);
                if (image.code.empty() || image.code.size() > 4096) throw std::runtime_error("No-reload cave exceeds page");
                SIZE_T done = 0;
                if (!WriteProcessMemory(process_, reinterpret_cast<void*>(address), image.code.data(), image.code.size(), &done) || done != image.code.size()) throw std::runtime_error("Cannot initialize no-reload cave");
                std::vector<std::uint8_t> actual(image.code.size());
                if (!ReadProcessMemory(process_, reinterpret_cast<void*>(address), actual.data(), actual.size(), &done) || done != actual.size() || actual != image.code) throw std::runtime_error("No-reload cave verification failed");
                DWORD old = 0;
                if (!VirtualProtectEx(process_, reinterpret_cast<void*>(address), 4096, PAGE_EXECUTE_READ, &old) || !FlushInstructionCache(process_, reinterpret_cast<void*>(address), image.code.size())) throw std::runtime_error("Cannot protect no-reload cave");
                images_.push_back(std::move(image));
            }
        } catch (...) {
            // No hook was published, so this allocation cannot be executing.
            VirtualFreeEx(process_, reinterpret_cast<void*>(allocation_), 0, MEM_RELEASE);
            throw;
        }
    }
    const std::vector<NoReloadClass>& classes() const { return classes_; }
    std::vector<Patch> code_ranges() const {
        std::vector<Patch> result;
        for (std::size_t i=0;i<images_.size();++i) result.push_back({allocation_+4096*i, images_[i].code, images_[i].code});
        return result;
    }
    std::vector<Patch> hooks() const {
        std::vector<Patch> result;
        for (const auto& image : images_) result.push_back(image.hook);
        return result;
    }
    Patch code_range() const { return code_ranges().front(); }
    bool publish(std::uint32_t mask) { return controls_.publish(mask); }
    std::uint32_t current_controls() const { return controls_.current(); }
    const Patch& hook() const { return images_.front().hook; }
};
}
