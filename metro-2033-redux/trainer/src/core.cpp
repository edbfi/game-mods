#include "metro/core.hpp"
#include "metro/no_reload.hpp"
#include <algorithm>
#include <limits>
#include <set>
#include <sstream>
#include <stdexcept>
#include <utility>

namespace metro {
namespace {
std::string trim(const std::string& s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return {};
    return s.substr(first, s.find_last_not_of(" \t\r\n") - first + 1);
}
bool read_equal(Memory& m, std::uintptr_t a, const std::vector<std::uint8_t>& expected) {
    std::vector<std::uint8_t> actual(expected.size());
    return m.read(a, actual) && actual == expected;
}
}
unsigned parse_hotkey(const std::string& text) {
    const auto separator = text.find_first_of("+-");
    if (separator != std::string::npos) {
        unsigned modifiers = 0;
        std::size_t start = 0, end = separator;
        while (end != std::string::npos) {
            const auto token = text.substr(start,end-start);
            const unsigned flag = token == "CTRL" ? 0x100 : token == "ALT" ? 0x200 : token == "SHIFT" ? 0x400 : 0;
            if (!flag || (modifiers & flag)) throw std::runtime_error("Invalid hotkey modifier: " + token);
            modifiers |= flag;
            start = end+1; end = text.find_first_of("+-",start);
        }
        const auto key = parse_hotkey(text.substr(start));
        if (!key || key > 255) throw std::runtime_error("Invalid hotkey chord: " + text);
        return modifiers | key;
    }
    if (text == "SPACE") return 0x20;
    if (text == "NONE") return 0;
    if (text.size() == 1 && ((text[0] >= 'A' && text[0] <= 'Z') || (text[0] >= '0' && text[0] <= '9'))) return static_cast<unsigned>(text[0]);
    if (text.size() >= 2 && text[0] == 'F') {
        for (unsigned n = 1; n <= 24; ++n) if (text == "F" + std::to_string(n)) return 0x6f + n;
    }
    const std::map<std::string, unsigned> names{{"INSERT",0x2d},{"DELETE",0x2e},{"HOME",0x24},{"END",0x23},{"PAGEUP",0x21},{"PAGEDOWN",0x22},{"PAUSE",0x13}};
    const auto found = names.find(text);
    if (found != names.end()) return found->second;
    throw std::runtime_error("Invalid hotkey: " + text);
}
bool Config::feature_enabled(const std::string& name) const {
    if (name == "unlimited_ammo") return unlimited_ammo;
    if (name == "unlimited_throwables") return unlimited_throwables;
    if (name == "unlimited_medkits") return unlimited_medkits;
    if (name == "unlimited_filters") return unlimited_filters;
    if (name == "indestructible_mask") return indestructible_mask;
    if (name == "no_reload") return no_reload;
    if (name == "god_mode") return god_mode;
    if (name == "unlimited_flashlight") return unlimited_flashlight;
    const auto found = extra_features.find(name);
    return found != extra_features.end() && found->second;
}
Config parse_config(const std::string& text) {
    Config result;
    result.hotkeys = {{"enabled",0x120},{"unlimited_ammo",0x170},{"unlimited_throwables",0x171},{"unlimited_medkits",0x172},{"unlimited_filters",0x173},{"indestructible_mask",0x174},{"no_reload",0x131},{"god_mode",0x132},{"unlimited_flashlight",0x133}};
    for (const auto& entry : verified_no_reload_classes()) {
        result.extra_features[entry.feature] = false;
        result.hotkeys[entry.feature] = 0;
    }
    std::string section, line;
    std::set<std::string> sections, keys;
    std::istringstream stream(text);
    while (std::getline(stream, line)) {
        line = trim(line);
        if (line.empty() || line.front() == ';' || line.front() == '#') continue;
        if (line.front() == '[' && line.back() == ']') {
            section = trim(line.substr(1, line.size() - 2));
            if ((section != "trainer" && section != "features" && section != "hotkeys") || !sections.insert(section).second)
                throw std::runtime_error("Unknown or duplicate INI section: " + section);
            continue;
        }
        const auto eq = line.find('=');
        if (section.empty() || eq == std::string::npos) throw std::runtime_error("Malformed INI entry: " + line);
        const auto key = trim(line.substr(0, eq));
        const auto value = trim(line.substr(eq + 1));
        if (!keys.insert(section + "." + key).second) throw std::runtime_error("Duplicate INI key: " + key);
        if (section == "hotkeys") {
            if (!result.hotkeys.count(key)) throw std::runtime_error("Unknown hotkey feature: " + key);
            result.hotkeys[key] = parse_hotkey(value);
            continue;
        }
        if (value != "true" && value != "false") throw std::runtime_error("Expected true or false: " + key);
        const bool on = value == "true";
        if (section == "trainer" && key == "enabled") result.enabled = on;
        else if (section == "features" && key == "unlimited_ammo") result.unlimited_ammo = on;
        else if (section == "features" && key == "unlimited_throwables") result.unlimited_throwables = on;
        else if (section == "features" && key == "unlimited_medkits") result.unlimited_medkits = on;
        else if (section == "features" && key == "unlimited_filters") result.unlimited_filters = on;
        else if (section == "features" && key == "indestructible_mask") result.indestructible_mask = on;
        else if (section == "trainer" && key == "sounds") result.sounds = on;
        else if (section == "features" && key == "no_reload") result.no_reload = on;
        else if (section == "features" && key == "god_mode") result.god_mode = on;
        else if (section == "features" && key == "unlimited_flashlight") result.unlimited_flashlight = on;
        else if (section == "features" && result.extra_features.count(key)) result.extra_features[key] = on;
        else if (section == "features" && key == "unlimited_currency") {
            if (on) throw std::runtime_error("Feature is not implemented: " + key);
        } else throw std::runtime_error("Unknown INI key: " + key);
    }
    std::set<unsigned> bindings;
    for (const auto& binding : result.hotkeys)
        if (binding.second && !bindings.insert(binding.second).second) throw std::runtime_error("Duplicate hotkey binding");
    return result;
}
Pattern parse_pattern(const std::string& text) {
    Pattern result;
    std::istringstream stream(text);
    std::string token;
    while (stream >> token) {
        if (token == "??" || token == "?") { result.push_back(-1); continue; }
        if (token.size() != 2 || token.find_first_not_of("0123456789abcdefABCDEF") != std::string::npos)
            throw std::runtime_error("Invalid byte pattern token: " + token);
        result.push_back(std::stoi(token, nullptr, 16));
    }
    if (result.empty()) throw std::runtime_error("Empty byte pattern");
    if (std::all_of(result.begin(), result.end(), [](int b) { return b == -1; }))
        throw std::runtime_error("Pattern must contain a concrete byte");
    return result;
}
std::vector<std::size_t> scan_pattern(const std::vector<std::uint8_t>& data, const Pattern& pattern) {
    if (pattern.empty() || std::any_of(pattern.begin(), pattern.end(), [](int b) { return b < -1 || b > 255; }))
        throw std::runtime_error("Invalid byte pattern");
    std::vector<std::size_t> matches;
    if (pattern.size() > data.size()) return matches;
    for (std::size_t i = 0; i <= data.size() - pattern.size(); ++i) {
        bool match = true;
        for (std::size_t j = 0; j < pattern.size(); ++j)
            if (pattern[j] != -1 && pattern[j] != data[i + j]) { match = false; break; }
        if (match) matches.push_back(i);
    }
    return matches;
}
PatchTransaction::PatchTransaction(std::vector<Patch> patches) : patches_(std::move(patches)) {
    pending_.reserve(patches_.size());
    for (std::size_t i = 0; i < patches_.size(); ++i) {
        const auto& p = patches_[i];
        if (p.original.empty() || p.original.size() != p.replacement.size() ||
            p.original.size() > std::numeric_limits<std::uintptr_t>::max() - p.address)
            throw std::runtime_error("Invalid patch region");
        for (std::size_t j = 0; j < i; ++j) {
            const auto& q = patches_[j];
            if (p.address < q.address + q.original.size() && q.address < p.address + p.original.size())
                throw std::runtime_error("Overlapping patch regions");
        }
    }
}
bool PatchTransaction::apply(Memory& memory, std::string& error) {
    error.clear();
    if (active()) { error = "Transaction already active or needs restoration"; return false; }
    for (const auto& p : patches_) {
        if (!read_equal(memory, p.address, p.original)) { error = "Original bytes do not match"; return false; }
    }
    for (std::size_t i = 0; i < patches_.size(); ++i) {
        const auto& p = patches_[i];
        // Revalidate immediately before writing, in addition to full preflight.
        if (!read_equal(memory, p.address, p.original)) {
            std::string rollback;
            restore(memory, rollback);
            error = "Original bytes changed before write" + (rollback.empty() ? std::string{} : "; " + rollback);
            return false;
        }
        pending_.push_back(i);
        if (!memory.write(p.address, p.replacement) || !read_equal(memory, p.address, p.replacement)) {
            std::string rollback;
            restore(memory, rollback);
            error = "Patch write failed" + (rollback.empty() ? std::string{} : "; " + rollback);
            return false;
        }
    }
    return true;
}
bool PatchTransaction::transition(Memory& memory, std::vector<Patch> desired, std::string& error) {
    PatchTransaction next(std::move(desired)); // Validate before any mutation.
    error.clear();
    transition_recovered_ = true;
    if (pending_.size() != patches_.size()) {
        transition_recovered_ = false;
        error = "Current transaction needs restoration";
        return false;
    }
    auto same = [](const Patch& a, const Patch& b) {
        return a.address == b.address && a.original == b.original && a.replacement == b.replacement;
    };
    std::vector<Patch> kept, removed, added;
    for (const auto& p : patches_) {
        if (!read_equal(memory, p.address, p.replacement)) {
            transition_recovered_ = false;
            error = "Owned bytes changed before transition";
            return false;
        }
        if (std::any_of(next.patches_.begin(), next.patches_.end(), [&](const Patch& q) { return same(p,q); })) kept.push_back(p);
        else removed.push_back(p);
    }
    for (const auto& p : next.patches_)
        if (!std::any_of(kept.begin(), kept.end(), [&](const Patch& q) { return same(p,q); })) added.push_back(p);
    PatchTransaction undo(std::move(removed)), install(std::move(added));
    for (std::size_t i=0;i<undo.patches_.size();++i) undo.pending_.push_back(i);
    // Build every nested byte vector and ownership slot before any write.
    std::vector<Patch> owned = kept;
    const auto undo_offset = owned.size();
    owned.insert(owned.end(),undo.patches_.begin(),undo.patches_.end());
    const auto install_offset = owned.size();
    owned.insert(owned.end(),install.patches_.begin(),install.patches_.end());
    std::vector<std::size_t> indices;
    indices.reserve(owned.size());
    for (std::size_t i=0;i<next.patches_.size();++i) next.pending_.push_back(i);
    bool ok = false;
    try {
        ok = undo.restore(memory, error);
        if (ok) ok = install.apply(memory, error);
    } catch (const std::exception&) { error = "Exception during patch transition"; }
    if (ok) {
        patches_.swap(next.patches_);
        pending_.swap(next.pending_);
        return true;
    }
    const auto failure = std::move(error);
    std::string recovery;
    // Remove additions before reapplying previous regions, which can overlap.
    transition_recovered_ = false;
    try {
        const bool additions_clean = install.restore(memory, recovery);
        const bool removals_clean = undo.restore(memory, recovery);
        transition_recovered_ = additions_clean && removals_clean && undo.apply(memory, recovery);
    } catch (const std::exception&) { }
    if (!transition_recovered_) {
        for (std::size_t i=0;i<kept.size();++i) indices.push_back(i);
        for (const auto i : undo.pending_) indices.push_back(undo_offset+i);
        for (const auto i : install.pending_) indices.push_back(install_offset+i);
        patches_.swap(owned);
        pending_.swap(indices);
    }
    error = failure + (transition_recovered_ ? "; previous selection restored" : "; recovery incomplete; ownership retained");
    return false;
}
bool PatchTransaction::restore(Memory& memory, std::string& error) {
    error.clear();
    std::vector<std::size_t> remaining;
    remaining.reserve(pending_.size());
    for (auto it = pending_.rbegin(); it != pending_.rend(); ++it) {
        const auto& p = patches_[*it];
        std::vector<std::uint8_t> current(p.original.size());
        bool ok = memory.read(p.address, current) && current.size() == p.original.size();
        if (ok) {
            // A failed write may leave a mixture of original/replacement bytes.
            // Only restore bytes still matching one of those owned states.
            for (std::size_t j = 0; j < current.size(); ++j)
                if (current[j] != p.original[j] && current[j] != p.replacement[j]) ok = false;
        }
        if (ok && current != p.original)
            ok = memory.write(p.address, p.original) && read_equal(memory, p.address, p.original);
        if (!ok) remaining.push_back(*it);
    }
    std::reverse(remaining.begin(), remaining.end());
    pending_ = std::move(remaining);
    if (active()) error = "Restoration incomplete: unreadable, foreign, or unwritable bytes remain";
    return !active();
}
}
