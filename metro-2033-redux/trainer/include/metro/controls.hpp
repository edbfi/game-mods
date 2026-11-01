// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/no_reload.hpp"
#include <array>
#include <map>
#include <string>

namespace metro {
inline constexpr unsigned no_reload_bit_offset = 7;
inline constexpr std::uint32_t no_reload_control_bits = 0x7fffff80;
// Stable layout: seven ordinary controls, then all 24 registered weapon classes.
// Bit 31 is reserved for the separate derived weapon-pressure control.
inline std::uint32_t effective_controls(bool master, const std::map<std::string, bool>& selected) {
    if (!master) return 0;
    const auto enabled = [&](const char* key) {
        const auto it = selected.find(key);
        return it != selected.end() && it->second;
    };
    constexpr std::array<const char*, 7> ordinary{{"unlimited_ammo", "unlimited_throwables",
        "unlimited_medkits", "unlimited_filters", "indestructible_mask", "god_mode", "unlimited_flashlight"}};
    std::uint32_t mask = 0;
    for (unsigned i = 0; i < ordinary.size(); ++i)
        if (enabled(ordinary[i])) mask |= std::uint32_t{1} << i;
    const auto classes = verified_no_reload_classes();
    if (classes.size() != 24) throw std::runtime_error("Control layout requires 24 weapon classes");
    for (unsigned i = 0; i < classes.size(); ++i)
        if (enabled("no_reload") || enabled(classes[i].feature))
            mask |= std::uint32_t{1} << (no_reload_bit_offset + i);
    return mask;
}
}
