// SPDX-License-Identifier: AGPL-3.0-only
#include "metro/presentation.hpp"
#include "metro/no_reload.hpp"
#include "metro/controls.hpp"
#include <algorithm>
#include <iostream>
#include <stdexcept>

namespace {
void check(bool value, const char* message) {
    if (!value) throw std::runtime_error(message);
}
void contains(const std::string& text, const std::string& needle) {
    check(text.find(needle) != std::string::npos, needle.c_str());
}
}
int main() {
    using namespace metro::ui;
    Model applied;
    applied.lifecycle = Lifecycle::Attached;
    const std::map<std::string, bool> selection{{"unlimited_ammo",true},{"no_reload",true},{"no_reload_ak74",false}};
    acknowledge(applied,true,selection,0);
    check(effective_label(applied,"unlimited_ammo") == "OFF","unacknowledged selection stays off");
    acknowledge(applied,true,selection,metro::effective_controls(true,selection));
    check(effective_label(applied,"unlimited_ammo") == "ON","acknowledged ordinary control");
    check(effective_label(applied,"no_reload_ak74") == "GLOBAL","acknowledged global control");
    contains(plain_text(render(applied,100,30)),"ACTIVE");
    acknowledge(applied,false,selection,0);
    check(applied.selected == selection,"pause retains selection");
    check(effective_label(applied,"no_reload_ak74") == "PAUSED","pause clears acknowledged controls");
    contains(plain_text(render(applied,100,30)),"PAUSED");
    applied.lifecycle = Lifecycle::Installing;
    contains(plain_text(render(applied,100,30)),"INACTIVE");
    contains(plain_text(render(applied,100,30)),"Installing");
    Model m;
    m.config = metro::parse_config("[hotkeys]\nunlimited_ammo=CTRL+SHIFT+F24\n");
    m.selected["unlimited_ammo"] = true;
    check(effective_label(m,"unlimited_ammo") == "OFF","waiting is not applied");
    m.lifecycle = Lifecycle::Attached;
    check(effective_label(m,"unlimited_ammo") == "PAUSED","selection survives master pause");
    check(effective_label(m,"god_mode") == "OFF","unselected stays off");
    m.master = true;
    check(effective_label(m,"unlimited_ammo") == "OFF","selection alone never implies application");
    m.effective["unlimited_ammo"] = true;
    check(effective_label(m,"unlimited_ammo") == "ON","committed application");
    m.effective["no_reload"] = true;
    check(effective_label(m,"no_reload_ak74") == "GLOBAL","global coverage");
    m.effective["no_reload_ak74"] = true;
    check(effective_label(m,"no_reload_ak74") == "ON","individual selection remains distinct");
    m.master = false; m.selected["no_reload"] = true; m.effective.clear();
    check(effective_label(m,"no_reload_tihar") == "PAUSED","global pause includes weapons");
    m.lifecycle = Lifecycle::Restoring;
    check(effective_label(m,"unlimited_ammo") == "OFF","cleared application stays off");
    m.effective["unlimited_ammo"] = true;
    check(effective_label(m,"unlimited_ammo") == "ON","restoring cannot falsely claim bytes restored");
    for (const auto& key : {"NONE","SPACE","INSERT","DELETE","HOME","END","PAGEUP","PAGEDOWN","PAUSE","A","0","F1","F24"}) {
        const auto label = binding_label(metro::parse_hotkey(key));
        check(!label.empty(),"every supported key has a label");
        check(label.find("VK(") == std::string::npos,"supported keys are human-readable");
    }
    check(binding_label(metro::parse_hotkey("CTRL+SHIFT+F24")) == "Ctrl+Shift+F24","configured modifiers");
    check(binding_label(metro::parse_hotkey("ALT+SPACE")) == "Alt+Space","default master label");
    check(binding_label(0) == "NONE","unbound is explicit");
    m.logfile = "C:/" + std::string(300,'x') + "/metro-trainer.log";
    m.error = "Could not restore memory. " + std::string(300,'x') + " Stop the game and restart.";
    m.status = "Changed ammo.";
    const auto full = render(m,100,30);
    check(full.dashboard && full.rows.size() == 30,"bounded dashboard");
    contains(plain_text(full),"Ctrl+Shift+F24");
    contains(plain_text(full),"metro-trainer.log");
    contains(plain_text(full),"Stop the game and restart.");
    check(diff(full,full).empty(),"idle has zero writes");
    for (const auto width : {std::size_t{72},std::size_t{100},std::size_t{160},std::size_t{100000}}) {
        for (const auto height : {std::size_t{24},std::size_t{25},std::size_t{60},std::size_t{100000}}) {
            auto frame = render(m,width,height);
            std::string pages;
            for (std::size_t page=0; page<frame.pages; ++page) {
                auto next = render(m,width,height,page);
                check(next.rows.size() <= std::min<std::size_t>(height,60),"height bound");
                for (const auto& row : next.rows) check(row.text.size() <= next.width,"width bound");
                const auto text = plain_text(next);
                contains(text,"Unlimited ammo"); contains(text,"Unlimited flashlight");
                pages += text;
            }
            for (const auto& weapon : metro::verified_no_reload_classes()) {
                std::string name = std::string(weapon.feature).substr(10);
                for (auto& c : name) if (c >= 'a' && c <= 'z') c = static_cast<char>(c-'a'+'A');
                contains(pages,name);
            }
            check(render(m,width,height,999).page == frame.pages-1,"out of range page clamped");
        }
    }
    for (const auto width : {std::size_t{0},std::size_t{1},std::size_t{20},std::size_t{71}}) {
        const auto small = render(m,width,4);
        check(!small.dashboard,"tiny uses plain snapshot");
        for (const auto& row : small.rows) check(row.text.size() <= small.width,"plain wraps");
        // Rejoin wrapped cells to ensure long paths/actions are not lost.
        std::string joined;
        for (const auto& row : small.rows) joined += row.text;
        contains(joined,m.logfile); contains(joined,m.error); contains(joined,"FLAMETHROWER");
        check(joined.find('\x1b') == std::string::npos,"no escapes");
    }
    check(!render(m,100,40,0,false).dashboard,"redirected is plain");
    Frame before{{{"long old content",Tone::Amber},{"vanished",Tone::Normal}},20};
    Frame after{{{"new",Tone::Normal}},20};
    const auto changes = diff(before,after);
    check(changes.size() == 1 && changes[0].rows.size() == 2,"adjacent changes batched");
    check(changes[0].rows[0].text == "new"+std::string(17,' '),"shortened row erases tail");
    check(changes[0].rows[1].text == std::string(20,' '),"vanished row erased");
    after.rows[0].tone = Tone::Error;
    check(!diff(Frame{{{"new",Tone::Normal}},20},after).empty(),"attribute-only changes count");
    check(!diff(after,after,true).empty(),"resize invalidates unchanged text");
    m.status = "unsafe\x1b[31m\ntext";
    const auto safe = plain_text(render(m,100,20));
    check(safe.find('\x1b') == std::string::npos,"control injection sanitized");
    std::cout << "Presentation tests passed\n";
}
