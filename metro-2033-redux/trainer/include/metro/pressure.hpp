// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/no_reload.hpp"
namespace metro {
enum class PressureSite { Recovery, Consumption1, Consumption2 };
// Weapon +0xc00: float pressure; +0x118: owner; +0x1b8: config.
// Config +0x1320: normal pressure; player global +0xfbd820 -> +0x38 -> +0xa0.
inline std::uint32_t helsing_class_id_rva() {
    for (const auto &entry : verified_no_reload_classes()) {
        if (std::string(entry.feature) == "no_reload_helsing" && entry.class_id_rvas.size() == 1)
            return entry.class_id_rvas.front();
    }
    throw std::runtime_error("Missing Helsing class binding");
}
// Control is an aligned DWORD, sampled once. Bit 31 is the derived pressure gate.
// Invalid/nonpositive/nonfinite normal pressure disables correction. NaN pressure
// is preserved; negative pressure (including -infinity) is floored.
inline NoReloadImage make_pressure_image(std::uintptr_t base, std::uintptr_t cave,
                                         std::uintptr_t control, PressureSite site) {
    if (site != PressureSite::Recovery && site != PressureSite::Consumption1 &&
        site != PressureSite::Consumption2)
        throw std::invalid_argument("invalid pressure site");
    if (control % 4)
        throw std::invalid_argument("unaligned pressure control");
    const bool recovery = site == PressureSite::Recovery;
    const std::uint32_t rva = recovery                             ? 0x2a9bb0
                              : site == PressureSite::Consumption1 ? 0x2b6727
                                                                   : 0x2b68f4;
    std::vector<std::uint8_t> original =
        recovery ? std::vector<std::uint8_t>{0x40, 0x53, 0x48, 0x83, 0xec, 0x20}
        : site == PressureSite::Consumption1
            ? std::vector<std::uint8_t>{0xf3, 0x0f, 0x10, 0x8b, 0,    0x0c, 0,   0,
                                        0x0f, 0x57, 0xc0, 0xf3, 0x0f, 0x5c, 0xce}
            : std::vector<std::uint8_t>{0xf3, 0x0f, 0x10, 0x83, 0,    0x0c,
                                        0,    0,    0xf3, 0x0f, 0x5c, 0xc7};
    std::vector<std::uint8_t> code;
    auto bytes = [&](std::initializer_list<std::uint8_t> v) { code.insert(code.end(), v); };
    auto emit_integer = [&](std::uint64_t v, unsigned count) {
        for (unsigned i = 0; i < count; ++i)
            code.push_back(v >> (i * 8));
    };
    std::vector<std::size_t> exits;
    auto exit_if = [&](unsigned cc) {
        bytes({0x0f, static_cast<std::uint8_t>(cc)});
        exits.push_back(code.size());
        emit_integer(0, 4);
    };
    if (!recovery)
        code.insert(code.end(), original.begin(), original.end());
    bytes({0x9c, 0x50, 0x52, 0x41, 0x53}); // flags, rax, rdx, r11
    bytes({0x48, 0x83, 0xec, 0x20, 0x0f, 0xae, 0x5c, 0x24, 0x10, 0xf3, 0x0f, 0x7f, 0x14,
           0x24}); // full xmm2
    // Native consumption above runs with the caller's MXCSR. Only correction
    // comparisons mask exceptions; retain DAZ/FTZ and restore every MXCSR bit.
    // Keep the saved value at +16 intact and use +20 for the masked copy.
    bytes({0x8b, 0x44, 0x24, 0x10, 0x0d, 0x80, 0x1f, 0, 0, 0x89, 0x44, 0x24, 0x14, 0x0f, 0xae, 0x54,
           0x24, 0x14});
    bytes({0x49, 0xbb});
    emit_integer(control, 8);
    bytes({0x41, 0x8b, 0x03, 0xa9});
    emit_integer(pressure_control_bit, 4);
    exit_if(0x84);
    bytes(recovery ? std::initializer_list<std::uint8_t>{0x49, 0x89, 0xcb}
                   : std::initializer_list<std::uint8_t>{0x49, 0x89, 0xdb});
    bytes({0x49, 0x8b, 0x83, 0x18, 1, 0, 0, 0x48, 0x85, 0xc0});
    exit_if(0x84);
    bytes({0x48, 0x8b, 0x10, 0x48, 0x85, 0xd2});
    exit_if(0x84);
    bytes({0x48, 0xb8});
    emit_integer(base + 0xfbd820, 8);
    bytes({0x48, 0x8b, 0, 0x48, 0x85, 0xc0});
    exit_if(0x84);
    bytes({0x48, 0x8b, 0x40, 0x38, 0x48, 0x85, 0xc0});
    exit_if(0x84);
    bytes({0x48, 0x05, 0xa0, 0, 0, 0, 0x48, 0x39, 0xc2});
    exit_if(0x85);
    bytes({0x49, 0x8b, 0x93, 0xb8, 1, 0, 0, 0x48, 0x85, 0xd2});
    exit_if(0x84);
    bytes({0x48, 0xb8});
    emit_integer(base + helsing_class_id_rva(), 8);
    bytes({0x8b, 0, 0x39, 0x02});
    exit_if(0x85);
    bytes({0x8b, 0x82, 0x20, 0x13, 0, 0, 0x85, 0xc0});
    exit_if(0x8e);
    bytes({0x3d, 0, 0, 0x80, 0x7f});
    exit_if(0x83);
    bytes({0xf3, 0x0f, 0x10, 0x92, 0x20, 0x13, 0, 0});
    if (recovery) {
        bytes({0x41, 0x0f, 0x2e, 0x93, 0, 0x0c, 0, 0});
        exit_if(0x8a);
        exit_if(0x86);
        bytes({0xf3, 0x41, 0x0f, 0x11, 0x93, 0, 0x0c, 0, 0});
    } else {
        const auto reg = site == PressureSite::Consumption1 ? 1 : 0;
        bytes({0x0f, 0x2e, static_cast<std::uint8_t>(0xd0 + reg)});
        exit_if(0x8a);
        exit_if(0x86);
        bytes({0xf3, 0x0f, 0x10, static_cast<std::uint8_t>(0xc2 + reg * 8)});
    }
    for (auto displacement_offset : exits) {
        auto displacement = static_cast<std::uint32_t>(code.size() - displacement_offset - 4);
        for (unsigned i = 0; i < 4; ++i)
            code[displacement_offset + i] = displacement >> (8 * i);
    }
    bytes({0x0f, 0xae, 0x54, 0x24, 0x10, 0xf3, 0x0f, 0x6f, 0x14, 0x24, 0x48, 0x83, 0xc4, 0x20, 0x41,
           0x5b, 0x5a, 0x58, 0x9d});
    if (recovery)
        code.insert(code.end(), original.begin(), original.end());
    bytes({0xff, 0x25, 0, 0, 0, 0});
    emit_integer(base + rva + original.size(), 8);
    auto distance = static_cast<std::int64_t>(cave) - static_cast<std::int64_t>(base + rva + 5);
    if (distance < INT32_MIN || distance > INT32_MAX)
        throw std::out_of_range("pressure cave range");
    std::vector<std::uint8_t> jump{0xe9};
    for (unsigned i = 0; i < 4; ++i)
        jump.push_back(static_cast<std::uint32_t>(distance) >> (8 * i));
    jump.resize(original.size(), 0x90);
    return {code, {base + rva, original, jump}};
}
struct PressureProfileSite {
    const char *name;
    std::uint32_t context_rva;
    const char *context;
    PressureSite site;
};
inline std::array<PressureProfileSite, 3> verified_pressure_sites() {
    return {{{"helsing_pressure_recovery", 0x2a9bb0,
              "40 53 48 83 ec 20 48 8b d9 48 81 c1 30 04 00 00 48 8b 01 ff 90 c8 01 00 00 84 c0 75 "
              "12 48 8b 03",
              PressureSite::Recovery},
             {"helsing_pressure_consumption1", 0x2b6727,
              "f3 0f 10 8b 00 0c 00 00 0f 57 c0 f3 0f 5c ce 0f 2f c8 f3 0f 11 8b 00 0c 00 00 73 1a "
              "c7 83 00 0c",
              PressureSite::Consumption1},
             {"helsing_pressure_consumption2", 0x2b68f4,
              "f3 0f 10 83 00 0c 00 00 f3 0f 5c c7 0f 2f c6 f3 0f 11 83 00 0c 00 00 73 16 48 8b 03 "
              "48 8b cb c7",
              PressureSite::Consumption2}}};
}

} // namespace metro
