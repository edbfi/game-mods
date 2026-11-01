// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/no_reload.hpp"
#include <array>
#include <map>
#include <string>

namespace metro {
inline std::uint32_t effective_controls(bool master, const std::map<std::string, bool>& selected, bool preserve_weapon_pressure = true) {
    if (!master) return 0;
    const auto enabled = [&](const char* key) {
        const auto it = selected.find(key);
        return it != selected.end() && it->second;
    };
    std::uint32_t mask = 0;
    for (unsigned i = 0; i < ordinary_controls.size(); ++i)
        if (enabled(ordinary_controls[i])) mask |= std::uint32_t{1} << i;
    const auto classes = verified_no_reload_classes();
    if (classes.size() != no_reload_class_count) throw std::runtime_error("Control layout requires 24 weapon classes");
    for (unsigned i = 0; i < classes.size(); ++i)
        if (enabled("no_reload") || enabled(classes[i].feature))
            mask |= std::uint32_t{1} << (no_reload_bit_offset + i);
    if (preserve_weapon_pressure && (enabled("unlimited_ammo") || enabled("no_reload") || enabled("no_reload_helsing")))
        mask |= pressure_control_bit;
    return mask;
}
}
