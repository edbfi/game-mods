// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/build_profile.hpp"
#include "metro/no_reload.hpp"
#include <algorithm>

namespace metro {
// Only spans verified against the captured supported-build runtime image.
// Move the two short-patch entries to complete preceding instructions.
struct ConditionalSite {
    const char* feature;
    std::uint32_t rva;
    std::vector<std::uint8_t> original;
    std::uint32_t context_rva;
    const char* context;
};
inline std::vector<ConditionalSite> verified_conditional_sites() {
    std::vector<ConditionalSite> sites;
    for (const auto& patch : verified_code_patches())
        sites.push_back({patch.feature, patch.rva, patch.original, patch.context_rva, patch.context});
    for (auto& site : sites) {
        if (site.rva == 0x1e371b) {
            site.rva = 0x1e3714;
            site.original = {0x80,0xb9,0x44,0x13,0,0,2};
        } else if (site.rva == 0x2b9a58) {
            site.rva = 0x2b9a55;
            site.original = {0x49,0x8b,0x06,0x2b,0xdf};
        }
    }
    return sites;
}
inline NoReloadImage make_conditional_image(std::uintptr_t base, std::uintptr_t cave,
        std::uintptr_t controls, const ConditionalSite& site) {
    constexpr const char* names[] = {"unlimited_ammo", "unlimited_throwables", "unlimited_medkits",
        "unlimited_filters", "indestructible_mask", "god_mode", "unlimited_flashlight"};
    const auto feature = std::find_if(std::begin(names), std::end(names),
        [&](const char* name) { return std::string_view(name) == site.feature; });
    if (feature == std::end(names) || controls % 4 || site.original.size() < 5)
        throw std::runtime_error("Invalid conditional hook profile");
    std::vector<std::uint8_t> code;
    auto bytes = [&](std::initializer_list<std::uint8_t> b) { code.insert(code.end(), b); };
    auto u64 = [&](std::uint64_t n) { for (unsigned i=0;i<8;++i) code.push_back(static_cast<std::uint8_t>(n>>(8*i))); };
    auto u32 = [&](std::uint32_t n) { for (unsigned i=0;i<4;++i) code.push_back(static_cast<std::uint8_t>(n>>(8*i))); };
    auto jump = [&](std::uintptr_t target) { bytes({0xff,0x25,0,0,0,0}); u64(target); };
    const auto continuation = base + site.rva + site.original.size();
    // One aligned DWORD snapshot. Restore RAX, RSP and flags before either path;
    // no SIMD state or other general registers are touched by the gate.
    bytes({0x9c,0x50,0x48,0xb8}); u64(controls);
    bytes({0x8b,0x00,0xa9}); u32(std::uint32_t{1} << (feature-std::begin(names)));
    bytes({0x0f,0x85}); const auto selected = code.size(); u32(0);
    auto path = [&](bool enabled) {
        bytes({0x58,0x9d});
        if (site.original.front() == 0xe8) {
            // Transfer with the exact original return address, shadow space and
            // stack alignment. Callee returns directly to the game, never the cave.
            if (enabled && std::string_view(site.feature) == "unlimited_flashlight") { jump(continuation); return; }
            std::uint32_t raw = 0;
            for (unsigned i=0;i<4;++i) raw |= std::uint32_t(site.original[i+1]) << (8*i);
            auto target = static_cast<std::uintptr_t>(static_cast<std::int64_t>(base+site.rva+5) + static_cast<std::int32_t>(raw));
            if (enabled) target += 0x30; // throwable action without consumption wrapper
            bytes({0xff,0x35,14,0,0,0}); jump(target); u64(continuation); return;
        }
        if (site.rva == 0x32b6e9 && !enabled) {
            bytes({0xff,0x35,6,0,0,0,0xff,0xa0,0xb0,1,0,0}); u64(continuation); return;
        }
        if (!enabled) code.insert(code.end(), site.original.begin(), site.original.end());
        else if (site.rva == 0x2aecd0) { bytes({0xb0,1,0xc3}); return; }
        else if (site.rva == 0x2b9a55) bytes({0x49,0x8b,0x06});
        else if (site.rva == 0x3292d1) bytes({0x45,0x0f,0x57,0xc9});
        else if (site.rva == 0x1e3714) { jump(base+0x1e372d); return; }
        jump(continuation);
    };
    path(false);
    const auto displacement = static_cast<std::uint32_t>(code.size()-(selected+4));
    for (unsigned i=0;i<4;++i) code[selected+i] = static_cast<std::uint8_t>(displacement>>(8*i));
    path(true);
    const auto distance = static_cast<std::int64_t>(cave)-static_cast<std::int64_t>(base+site.rva+5);
    if (distance < INT32_MIN || distance > INT32_MAX) throw std::runtime_error("Conditional cave outside jump range");
    std::vector<std::uint8_t> hook{0xe9};
    for (unsigned i=0;i<4;++i) hook.push_back(static_cast<std::uint8_t>(static_cast<std::uint32_t>(distance)>>(8*i)));
    hook.resize(site.original.size(),0x90);
    return {code,{base+site.rva,site.original,hook}};
}
}
