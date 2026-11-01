#pragma once
#include <cstdint>
#include <string_view>
#include <vector>

namespace metro {
inline constexpr std::string_view supported_sha256 =
    "7c35095f27beab83b31914029eca0da07991bba8630ec12ba97eec43ffc60c38";
inline constexpr std::string_view expected_runtime_text_sha256 =
    "1f82b1598507d28b1dde205f8f41ec659b4d08b1ea5bcbdb36311941f1c73e93";
// Only evidence-backed, build-specific profiles belong here.
struct DataFlag {
    const char* feature;
    std::uint32_t rva;
    std::vector<std::uint8_t> original;
    std::vector<std::uint8_t> enabled;
};
struct CodePatchDefinition {
    const char* feature;
    std::uint32_t rva;
    std::vector<std::uint8_t> original;
    std::uint32_t context_rva;
    const char* context;
    std::vector<std::uint8_t> replacement;
};
}
#include "metro/survival_patches.hpp"
namespace metro {
inline std::vector<CodePatchDefinition> verified_code_patches() {
    std::vector<CodePatchDefinition> patches{{"unlimited_ammo", 0x2aecd0, {0x48,0x89,0x5c,0x24,0x08},
        0x2aecd0, "48 89 5C 24 08 57 48 83 EC 20 80 B9 F0 0B 00 00 00 48 8B D9",
        {0xb0,0x01,0xc3,0x90,0x90}},
        // The Hellsing's own reload never asks the predicate above: keep its arrow count.
        {"unlimited_ammo", 0x2b9a58, {0x2b,0xdf},
        0x2b9a52, "41 FF D1 49 8B 06 2B DF 8B D3 49 8B CE FF 90 F8 01 00 00",
        {0x90,0x90}},
        {"unlimited_throwables", 0x2e870a, {0xe8,0x91,0x36,0xf6,0xff},
        0x2e86ff, "48 8d 96 18 01 00 00 4c 89 70 e8 e8 91 36 f6 ff 48 8b 8e 18 01",
        {0xe8,0xc1,0x36,0xf6,0xff}},
        {"unlimited_throwables", 0x348b92, {0xe8,0x09,0x32,0xf0,0xff},
        0x348b87, "c1 60 0a 00 00 41 8b f8 4c 8b e2 e8 09 32 f0 ff 49 8d b6 b0 05",
        {0xe8,0x39,0x32,0xf0,0xff}},
        {"unlimited_filters", 0x3292d1, {0xf3,0x44,0x0f,0x59,0xca},
        0x3292d1, "F3 44 0F 59 CA 44 0F 28 C0 F3 44 0F 5E 86 54 04 00 00 F3 41 0F 5C C1",
        {0x45,0x0f,0x57,0xc9,0x90}},
        {"indestructible_mask", 0x32b6e9, {0xff,0x90,0xb0,0x01,0x00,0x00},
        0x32b6e9, "FF 90 B0 01 00 00 0F 57 C0 0F 2F 83 08 03 00 00 72 48",
        {0x90,0x90,0x90,0x90,0x90,0x90}},
        {"unlimited_medkits", 0x33079e, {0x41,0x21,0x80,0x78,0x04,0x00,0x00},
        0x330795, "b8 01 00 00 00 d3 e0 f7 d0 41 21 80 78 04 00 00 41 c7 80 84 04 00 00 ff 00 00 00",
        {0x90,0x90,0x90,0x90,0x90,0x90,0x90}}};
    append_survival_patches(patches);
    return patches;
}
inline std::vector<DataFlag> verified_flags() { return {}; }
}
