#pragma once

// Included by build_profile.hpp after CodePatchDefinition is declared.
// Evidence and limitations: docs/survival-analysis.md.
namespace metro {
inline void append_survival_patches(std::vector<CodePatchDefinition>& patches) {
    patches.push_back({"god_mode", 0x1e371b, {0x75,0x1a},
        0x1e3710, "48 83 EC 28 80 B9 44 13 00 00 02 75 1A 85 D2 74 0C 48 8D 0D F8 F2 93 00",
        {0xeb,0x10}});
    patches.push_back({"unlimited_flashlight", 0x1e982e, {0xe8,0xfd,0x7e,0x14,0x00},
        0x1e981b, "48 8B CB F3 0F 59 48 78 0F 5B D2 F3 0F 59 15 42 F6 C7 00 E8 FD 7E 14 00 0F 28 CE",
        {0x90,0x90,0x90,0x90,0x90}});
    // Night vision spends the same battery; preserve charge while it is on too.
    patches.push_back({"unlimited_flashlight", 0x1e98f2, {0xe8,0x39,0x7e,0x14,0x00},
        0x1e98e4, "F3 41 0F 59 F9 4C 8B CE 48 8B CB 0F 28 CF E8 39 7E 14 00 48 8B 07 0F 28 CE",
        {0x90,0x90,0x90,0x90,0x90}});
}
}
