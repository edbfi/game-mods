// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include <array>
#include <cstdint>

namespace metro {
inline constexpr std::array<const char*, 7> ordinary_controls{{"unlimited_ammo", "unlimited_throwables",
    "unlimited_medkits", "unlimited_filters", "indestructible_mask", "god_mode", "unlimited_flashlight"}};
inline constexpr unsigned no_reload_bit_offset = 7;
inline constexpr unsigned no_reload_class_count = 24;
inline constexpr std::uint32_t no_reload_control_bits = 0x7fffff80;
// Bit 31 remains reserved for the separate derived weapon-pressure control.
}
