#include "metro/core.hpp"
#include "metro/controls.hpp"
#include <algorithm>
#include <iostream>
#include <stdexcept>
#include <utility>

namespace {
void check(bool condition) { if (!condition) throw std::runtime_error("check failed"); }
template<class F> void rejects(F f) {
    bool threw = false;
    try { f(); } catch (const std::runtime_error&) { threw = true; }
    check(threw);
}
struct FakeMemory : metro::Memory {
    std::vector<std::uint8_t> bytes{1,2,3,4,5,6};
    int writes = 0;
    int fail_write = -1;
    bool fail_restoration = false;
    bool read(std::uintptr_t a, std::vector<std::uint8_t>& out) override {
        if (a > bytes.size() || out.size() > bytes.size() - a) return false;
        std::copy_n(bytes.begin() + a, out.size(), out.begin());
        return true;
    }
    bool write(std::uintptr_t a, const std::vector<std::uint8_t>& in) override {
        if (a > bytes.size() || in.size() > bytes.size() - a) return false;
        ++writes;
        if (fail_restoration && in[0] == 1) return false;
        if (writes == fail_write) { bytes[a] = in[0]; return false; }
        std::copy(in.begin(), in.end(), bytes.begin() + a);
        return true;
    }
};
}
int main() {
    check(metro::parse_config("").enabled);
    check(!metro::parse_config("[trainer]\nenabled=false\n").enabled);
    check(!metro::parse_config("").unlimited_filters && !metro::parse_config("").indestructible_mask);
    const auto gas = metro::parse_config("[features]\nunlimited_filters=true\nindestructible_mask=true\n");
    check(gas.unlimited_filters && gas.indestructible_mask && !gas.unlimited_ammo);
    const auto cfg = metro::parse_config("; comment\n[trainer]\nenabled=true\n[features]\nunlimited_ammo=true\nunlimited_medkits=false\nno_reload=false\n");
    check(cfg.enabled && cfg.unlimited_ammo && !cfg.unlimited_medkits && !cfg.unlimited_throwables);
    for (const auto& bad : {"enabled=true", "[other]", "[trainer]\nenabled=1", "[features]\nunknown=false", "[trainer]\nenabled=true\nenabled=false", "[features]\n[features]"})
        rejects([&] { metro::parse_config(bad); });
    const auto modern = metro::parse_config("[features]\nno_reload=true\ngod_mode=true\nunlimited_flashlight=true\n[hotkeys]\nenabled=HOME\nno_reload=NONE\n");
    const auto individual = metro::parse_config("[features]\nno_reload_ak74=true\n[hotkeys]\nno_reload_ak74=F9\n");
    check(individual.feature_enabled("no_reload_ak74") && individual.hotkeys.at("no_reload_ak74") == 0x78);
    check(modern.no_reload && modern.god_mode && modern.unlimited_flashlight && modern.hotkeys.at("enabled") == 0x24);
    check(metro::parse_hotkey("F24") == 0x87 && metro::parse_hotkey("A") == 65);
    rejects([] { metro::parse_config("[hotkeys]\nenabled=ALT+F1"); });
    rejects([] { metro::parse_config("[hotkeys]\nenabled=CTRL+CTRL+F1"); });
    check(metro::parse_hotkey("CTRL-1") == 0x131 && metro::parse_hotkey("CTRL+SHIFT+F1") == 0x570);
    check(metro::parse_hotkey("CTRL+SPACE") == 0x120);
    metro::KeyLatch latch;
    check(!latch.update(false) && latch.update(true) && !latch.update(true) && !latch.update(false) && latch.update(true));
    check(metro::scan_pattern({1,2,1,3}, metro::parse_pattern("01 ??")) == std::vector<std::size_t>({0,2}));
    check(metro::scan_pattern({1}, metro::parse_pattern("01 02")).empty());
    check(metro::scan_pattern({1,1,1}, metro::parse_pattern("01 01")) == std::vector<std::size_t>({0,1}));
    for (const auto& bad : {"", "?? ?", "GG", "100", "0x01"}) rejects([&] { metro::parse_pattern(bad); });
    rejects([] { metro::PatchTransaction t({{0,{1,2},{8,9}}, {1,{2},{9}}}); });
    rejects([] { metro::PatchTransaction t({{0,{1},{8,9}}}); });
    FakeMemory memory;
    const auto original = memory.bytes;
    std::string error;
    metro::PatchTransaction t({{0,{1,2},{8,9}}, {4,{5,6},{7,8}}});
    check(t.apply(memory, error));
    check(!t.apply(memory, error));
    check(t.restore(memory, error) && memory.bytes == original && !t.active());
    memory.bytes[4] = 10;
    const int previous_writes = memory.writes;
    check(!t.apply(memory, error) && memory.writes == previous_writes);
    memory.bytes = original;
    memory.fail_write = memory.writes + 2;
    check(!t.apply(memory, error) && memory.bytes == original && !t.active());
    memory.fail_write = memory.writes + 1;
    memory.fail_restoration = true;
    check(!t.apply(memory, error) && t.active() && !error.empty());
    memory.fail_restoration = false;
    check(t.restore(memory, error) && memory.bytes == original);
    memory.fail_write = -1;
    check(t.apply(memory, error));
    memory.bytes[0] = 42;
    check(!t.restore(memory, error) && t.active() && memory.bytes[0] == 42);
    check(memory.bytes[4] == original[4]); // Independent owned region still restored.
    memory.bytes[0] = 8;
    check(t.restore(memory, error) && memory.bytes == original);
    FakeMemory differential;
    metro::PatchTransaction changes({{0,{1,2},{8,9}}});
    check(changes.apply(differential,error));
    int writes = differential.writes;
    check(changes.transition(differential, {{0,{1,2},{8,9}}, {4,{5,6},{7,8}}}, error));
    check(differential.writes == writes+1); // Retained feature untouched.
    writes = differential.writes;
    check(changes.transition(differential, {{0,{1,2},{8,9}}, {4,{5,6},{7,8}}}, error));
    check(differential.writes == writes); // Effective selection unchanged.
    differential.fail_write = differential.writes+2;
    check(!changes.transition(differential, {{0,{1,2},{6,7}}, {4,{5,6},{7,8}}}, error));
    check(changes.transition_recovered() && differential.bytes == std::vector<std::uint8_t>({8,9,3,4,7,8}));
    differential.fail_write = -1;
    differential.bytes[4] = 42;
    writes = differential.writes;
    check(!changes.transition(differential, {{0,{1,2},{6,7}}, {4,{5,6},{7,8}}}, error));
    check(!changes.transition_recovered() && differential.writes == writes && differential.bytes[4] == 42);
    differential.bytes[4] = 7;
    check(changes.restore(differential,error) && differential.bytes == original);
    FakeMemory recovery_memory;
    metro::PatchTransaction recovery_test({{0,{1,2},{8,9}}});
    check(recovery_test.apply(recovery_memory,error));
    recovery_memory.fail_restoration = true;
    check(!recovery_test.transition(recovery_memory, {}, error) && !recovery_test.transition_recovered() && recovery_test.active());
    recovery_memory.fail_restoration = false;
    check(recovery_test.restore(recovery_memory,error) && recovery_memory.bytes == original);
    metro::PatchTransaction empty({});
    check(empty.transition(recovery_memory, {{0,{1,2},{8,9}}}, error));
    const std::vector<metro::Patch> changed_mask{{0,{1,2},{6,7}}};
    check(empty.transition(recovery_memory, changed_mask, error));
    check(recovery_memory.bytes[0] == 6);
    check(empty.transition(recovery_memory, {}, error) && !empty.active() && recovery_memory.bytes == original);
    check(metro::parse_config("[trainer]\npreserve_weapon_pressure=true\n").preserve_weapon_pressure);
    check(!metro::parse_config("[trainer]\npreserve_weapon_pressure=false\n").preserve_weapon_pressure);
    rejects([] { metro::parse_config("[trainer]\npreserve_weapon_pressure=invalid\n"); });
    for (bool master : {false, true})
        for (bool preserve : {false, true})
            for (const auto* feature : {"unlimited_ammo", "no_reload", "no_reload_helsing", "no_reload_tihar"}) {
                const auto mask = metro::effective_controls(master, {{feature, true}}, preserve);
                check(bool(mask & metro::pressure_control_bit) ==
                    (master && preserve && std::string(feature) != "no_reload_tihar"));
            }
    std::cout << "Core tests passed\n";
}
