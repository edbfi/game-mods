// SPDX-License-Identifier: AGPL-3.0-only
#include "metro/presentation.hpp"
#include "metro/no_reload.hpp"
#include "metro/controls.hpp"
#include <algorithm>
#include <array>
#include <iomanip>
#include <sstream>

namespace metro::ui {
namespace {
const std::array<std::pair<const char*, const char*>, 8> primary{{
    {"unlimited_ammo", "Unlimited ammo"}, {"unlimited_throwables", "Unlimited throwables"},
    {"unlimited_medkits", "Unlimited medkits"}, {"unlimited_filters", "Unlimited filters"},
    {"indestructible_mask", "Indestructible mask"}, {"no_reload", "No reload (all weapons)"},
    {"god_mode", "God mode"}, {"unlimited_flashlight", "Unlimited flashlight"}}};
bool flag(const std::map<std::string, bool>& values, const std::string& key) {
    const auto it = values.find(key);
    return it != values.end() && it->second;
}
std::string clean(std::string text) {
    // Byte-safe ASCII cells: logs/paths cannot inject terminal control sequences.
    for (auto& c : text) if (static_cast<unsigned char>(c) < 32 || static_cast<unsigned char>(c) > 126) c = '?';
    return text;
}
std::string lifecycle(Lifecycle value) {
    switch (value) {
    case Lifecycle::Waiting: return "Waiting for game";
    case Lifecycle::Preparing: return "Preparing";
    case Lifecycle::Installing: return "Installing disabled hooks";
    case Lifecycle::Attached: return "Attached";
    case Lifecycle::Restoring: return "Restoring original bytes";
    case Lifecycle::Stopped: return "Stopped";
    case Lifecycle::Error: return "Needs attention";
    }
    return "Unknown";
}
unsigned binding(const Model& m, const std::string& key) {
    const auto it = m.config.hotkeys.find(key);
    return it == m.config.hotkeys.end() ? 0 : it->second;
}
std::string entry(const Model& m, const std::string& key, const std::string& name, bool columns) {
    std::ostringstream out;
    if (columns) out << "  " << std::left << std::setw(24) << name << std::setw(20) << binding_label(binding(m,key))
                     << std::setw(10) << (flag(m.selected,key) ? "ON" : "OFF") << effective_label(m,key);
    else out << name << " [" << binding_label(binding(m,key)) << "] selected="
             << (flag(m.selected,key) ? "ON" : "OFF") << " effective=" << effective_label(m,key);
    return out.str();
}
}
void acknowledge(Model& model, bool master, const std::map<std::string, bool>& selected,
                 std::uint32_t controls) {
    model.master = master;
    model.selected = selected;
    model.effective.clear();
    for (const auto& feature : selected) {
        const auto bit = effective_controls(true, {{feature.first, true}});
        model.effective[feature.first] = feature.second && bit && (controls & bit) == bit;
    }
}
std::string binding_label(unsigned value) {
    if (!value) return "NONE";
    std::string result;
    if (value & 0x100) result += "Ctrl+";
    if (value & 0x200) result += "Alt+";
    if (value & 0x400) result += "Shift+";
    const auto key = value & 0xff;
    if ((key >= 'A' && key <= 'Z') || (key >= '0' && key <= '9')) return result + static_cast<char>(key);
    if (key >= 0x70 && key <= 0x87) return result + "F" + std::to_string(key - 0x6f);
    const std::map<unsigned,std::string> names{{0x20,"Space"},{0x2d,"Insert"},{0x2e,"Delete"},{0x24,"Home"},
        {0x23,"End"},{0x21,"PageUp"},{0x22,"PageDown"},{0x13,"Pause"}};
    const auto it = names.find(key);
    return result + (it == names.end() ? "VK(" + std::to_string(key) + ")" : it->second);
}
std::string effective_label(const Model& m, const std::string& key) {
    if (flag(m.effective,key)) return "ON";
    if (key.rfind("no_reload_",0) == 0 && flag(m.effective,"no_reload")) return "GLOBAL";
    if (m.lifecycle == Lifecycle::Attached && !m.master)
        return flag(m.selected,key) || (key.rfind("no_reload_",0) == 0 && flag(m.selected,"no_reload")) ? "PAUSED" : "OFF";
    return "OFF";
}
Frame render(const Model& m, std::size_t width, std::size_t height, std::size_t page, bool interactive) {
    Frame f;
    f.width = std::clamp<std::size_t>(width, 1, 160);
    height = std::min<std::size_t>(height,60);
    f.dashboard = interactive && f.width >= 72 && height >= 24;
    auto add = [&](std::string text, Tone tone = Tone::Normal) {
        text = clean(std::move(text));
        if (f.dashboard) {
            if (text.size() > f.width) text = text.substr(0,f.width-3) + "...";
            f.rows.push_back({std::move(text),tone});
        } else {
            if (text.empty()) f.rows.push_back({{},tone});
            for (std::size_t p=0; p<text.size(); p+=f.width) f.rows.push_back({text.substr(p,f.width),tone});
        }
    };
    if (f.dashboard) {
        add("   ___  __| | |__ / _(_)   METRO 2033 REDUX",Tone::Amber);
        add("  / -_)/ _` | '_ \\  _| |   trainer / edbfi",Tone::Amber);
        add("  \\___|\\__,_|_.__/_| |_|",Tone::Amber);
    } else add("edbfi / Metro 2033 Redux trainer",Tone::Amber);
    add("Trainer " + std::string(m.lifecycle == Lifecycle::Attached ? (m.master ? "ACTIVE" : "PAUSED") : "INACTIVE") + " [" + binding_label(binding(m,"enabled")) + "]  /  " + lifecycle(m.lifecycle),Tone::Amber);
    add(f.dashboard ? "  Feature                 Binding             Selected  Effective" : "Features (selected / effective)",Tone::Muted);
    for (const auto& item : primary) add(entry(m,item.first,item.second,f.dashboard));
    const auto weapons = verified_no_reload_classes();
    const auto capacity = f.dashboard ? height - 20 : weapons.size();
    f.pages = (weapons.size()+capacity-1)/capacity;
    f.page = std::min(page,f.pages-1);
    add("No reload / weapons  " + std::to_string(f.page+1) + "/" + std::to_string(f.pages)
        + (f.dashboard ? "  (console focus: Left / Right)" : ""),Tone::Amber);
    for (std::size_t i=f.page*capacity; i<std::min(weapons.size(),(f.page+1)*capacity); ++i) {
        std::string name = std::string(weapons[i].feature).substr(10);
        for (auto& c : name) if (c >= 'a' && c <= 'z') c = static_cast<char>(c - 'a' + 'A');
        add(entry(m,weapons[i].feature,name,f.dashboard));
    }
    add("ON=applied  PAUSED=master off  GLOBAL=all-weapons no reload",Tone::Muted);
    add("Log: " + (m.logfile.empty() ? "not set" : m.logfile),Tone::Muted);
    add("Status: " + m.status);
    add(m.error.empty() ? "Ctrl+C: restore and exit" : "Error: " + m.error, m.error.empty() ? Tone::Muted : Tone::Error);
    // Keep the file suffix/recovery action visible even for long dashboard messages.
    if (f.dashboard) {
        for (const auto index : {f.rows.size()-3,f.rows.size()-1}) {
            const auto full = clean(index == f.rows.size()-3 ? "Log: " + m.logfile : "Error: " + m.error);
            if (full.size() > f.width) f.rows[index].text = full.substr(0,7) + "..." + full.substr(full.size()-(f.width-10));
        }
        f.rows.resize(height);
    }
    return f;
}
std::vector<DirtyRows> diff(const Frame& previous, const Frame& next, bool invalidate) {
    const auto width = std::max(previous.width,next.width);
    std::vector<DirtyRows> result;
    for (std::size_t i=0; i<std::max(previous.rows.size(),next.rows.size()); ++i) {
        const Row before = i<previous.rows.size() ? previous.rows[i] : Row{};
        Row after = i<next.rows.size() ? next.rows[i] : Row{};
        if (!invalidate && previous.width == next.width && before == after) continue;
        after.text.resize(width,' ');
        if (result.empty() || result.back().first+result.back().rows.size() != i) result.push_back({i,{}});
        result.back().rows.push_back(std::move(after));
    }
    return result;
}
std::string plain_text(const Frame& frame) {
    std::string result;
    for (const auto& row : frame.rows) result += row.text + '\n';
    return result;
}
}
