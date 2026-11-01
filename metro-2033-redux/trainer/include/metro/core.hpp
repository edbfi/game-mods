#pragma once
#include <cstddef>
#include <cstdint>
#include <string>
#include <map>
#include <vector>

namespace metro {
struct Config {
    bool enabled = true;
    bool unlimited_ammo = false;
    bool unlimited_throwables = false;
    bool unlimited_medkits = false;
    bool unlimited_filters = false;
    bool indestructible_mask = false;
    bool no_reload = false;
    bool god_mode = false;
    bool unlimited_flashlight = false;
    bool sounds = true;
    std::map<std::string, bool> extra_features;
    std::map<std::string, unsigned> hotkeys;
    bool feature_enabled(const std::string& name) const;
};
// Portable virtual-key parser and rising-edge latch (held keys never repeat).
unsigned parse_hotkey(const std::string& text);
class KeyLatch {
    bool down_ = false;
public:
    bool update(bool down) { const bool pressed = down && !down_; down_ = down; return pressed; }

};
// Throws std::runtime_error for malformed, duplicate, or unsupported settings.
Config parse_config(const std::string& text);
using Pattern = std::vector<int>; // -1 means wildcard, otherwise one byte.
Pattern parse_pattern(const std::string& text);
std::vector<std::size_t> scan_pattern(const std::vector<std::uint8_t>& data,
                                    const Pattern& pattern);
struct Memory {
    virtual ~Memory() = default;
    virtual bool read(std::uintptr_t address, std::vector<std::uint8_t>& out) = 0;
    // A false return may still have changed any subset of the requested bytes.
    virtual bool write(std::uintptr_t address, const std::vector<std::uint8_t>& bytes) = 0;
};
struct Patch {
    std::uintptr_t address;
    std::vector<std::uint8_t> original;
    std::vector<std::uint8_t> replacement;
};
class PatchTransaction {
public:
    explicit PatchTransaction(std::vector<Patch> patches);
    bool apply(Memory& memory, std::string& error);
    bool restore(Memory& memory, std::string& error);
    // Retained patches are validated but never rewritten. On failure ownership
    // remains tracked; transition_recovered() reports a complete prior state.
    bool transition(Memory& memory, std::vector<Patch> desired, std::string& error);
    bool transition_recovered() const { return transition_recovered_; }
    bool active() const { return !pending_.empty(); }
private:
    bool transition_recovered_ = true;
    std::vector<Patch> patches_;
    std::vector<std::size_t> pending_;
};
}
