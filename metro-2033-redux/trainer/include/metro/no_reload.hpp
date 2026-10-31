#pragma once
#include "metro/core.hpp"
#include <cstdint>
#include <limits>
#include <stdexcept>
#include <vector>

namespace metro {
// Named configuration-ID globals established from this build's registrations.
struct NoReloadClass { const char* feature; std::vector<std::uint32_t> class_id_rvas; };
inline std::vector<NoReloadClass> verified_no_reload_classes() {
    return {
        {"no_reload_ak74",{0xf5d090}},
        {"no_reload_aksu",{0xf5d094}},
        {"no_reload_rpk",{0xf5d098}},
        {"no_reload_ashot",{0xf5d09c}},
        {"no_reload_vsv",{0xf5d0a0}},
        {"no_reload_ventil",{0xf5d0a4}},
        {"no_reload_padonag",{0xf5d0a8}},
        {"no_reload_preved",{0xf5d0ac}},
        {"no_reload_vyhlop",{0xf5d0b0}},
        {"no_reload_saiga",{0xf5d0b4}},
        {"no_reload_tumak",{0xf5d0b8}},
        {"no_reload_2012",{0xf5d0bc}},
        {"no_reload_gatling",{0xf5d0c0}},
        {"no_reload_ubludok",{0xf5d0c4}},
        {"no_reload_uboynicheg",{0xf5d0c8}},
        {"no_reload_bigun",{0xf5d0cc}},
        {"no_reload_duplet",{0xf5d0d0,0xf5d20c,0xf5d210,0xf5d214}},
        {"no_reload_revolver",{0xf5d0d4}},
        {"no_reload_abzac",{0xf5d0d8}},
        {"no_reload_hellbreath",{0xf5d0dc}},
        {"no_reload_hellshock",{0xf5d0e0}},
        {"no_reload_tihar",{0xf5d0e4}},
        {"no_reload_helsing",{0xf5d0e8}},
        {"no_reload_flamethrower",{0xf5d0f8}}};
}
struct NoReloadImage { std::vector<std::uint8_t> code; Patch hook; };
enum class NoReloadSiteKind { Shared, DirectCallback, EventCallback };
struct NoReloadSite {
    NoReloadSiteKind kind;
    std::uint32_t rva;
    const char* context;
    std::vector<std::uint8_t> original;
};
inline std::vector<NoReloadSite> verified_no_reload_sites() {
    return {
      {NoReloadSiteKind::Shared,0x2a9a0c,"49 8B 06 41 2B FD 8B D7 49 8B CE FF 90 60 13 00 00",{0x49,0x8b,0x06,0x41,0x2b,0xfd}},
      {NoReloadSiteKind::DirectCallback,0x2c6ef4,"8D 50 FF FF 93 60 13 00 00 69 97 20 0C 00 00 E8 03 00 00",{0x8d,0x50,0xff,0xff,0x93,0x60,0x13,0x00,0x00}},
      {NoReloadSiteKind::EventCallback,0x2c6fab,"8D 50 FF 48 8B 46 F8 FF 90 60 13 00 00 69 96 F8 0B 00 00 E8 03 00 00",{0x8d,0x50,0xff,0x48,0x8b,0x46,0xf8}}};
}
inline constexpr std::uint32_t no_reload_hook_rva = 0x2a9a0c;
inline constexpr const char* no_reload_context = "49 8B 06 41 2B FD 8B D7 49 8B CE FF 90 60 13 00 00";
// One control byte per class; nonzero suppresses that class's firing decrement.
// Caller must validate context, write/verify cave before installing hook, and
// retain cave allocation until process exit even after restoring the hook.
inline NoReloadImage make_no_reload_image(std::uintptr_t base,
        std::uintptr_t cave, std::uintptr_t controls,
        const std::vector<NoReloadClass>& classes,
        const NoReloadSite& site = verified_no_reload_sites().front()) {
    std::vector<std::uint8_t> code;
    auto bytes=[&](std::initializer_list<std::uint8_t> b){code.insert(code.end(),b);};
    auto u64=[&](std::uint64_t n){for(unsigned i=0;i<8;++i)code.push_back(static_cast<std::uint8_t>(n>>(8*i)));};
    auto u32=[&](std::uint32_t n){for(unsigned i=0;i<4;++i)code.push_back(static_cast<std::uint8_t>(n>>(8*i)));};
    auto branch=[&](std::uint8_t condition){bytes({0x0f,condition});auto p=code.size();u32(0);return p;};
    auto fix=[&](std::size_t p,std::size_t target){auto d=static_cast<std::int32_t>(target)-static_cast<std::int32_t>(p+4);for(unsigned i=0;i<4;++i)code[p+i]=static_cast<std::uint8_t>(static_cast<std::uint32_t>(d)>>(8*i));};
    std::vector<std::size_t> normal, selected;
    bytes({0x9c,0x50,0x51,0x52}); // flags, rax, rcx, rdx
    if(site.kind==NoReloadSiteKind::Shared) bytes({0x49,0x8b,0x86,0x18,0x01,0x00,0x00});
    else bytes({0x48,0x8b,0x81,0x18,0x01,0x00,0x00});
    bytes({0x48,0x85,0xc0});
    normal.push_back(branch(0x84));
    bytes({0x48,0x8b,0x10,0x48,0x85,0xd2});normal.push_back(branch(0x84));
    bytes({0x48,0xb8});u64(base+0xfbd820);
    bytes({0x48,0x8b,0x00,0x48,0x85,0xc0});normal.push_back(branch(0x84));
    bytes({0x48,0x8b,0x40,0x38,0x48,0x85,0xc0});normal.push_back(branch(0x84));
    bytes({0x48,0x05,0xa0,0x00,0x00,0x00,0x48,0x39,0xc2});normal.push_back(branch(0x85));
    if(site.kind==NoReloadSiteKind::Shared) bytes({0x49,0x8b,0x86,0xb8,0x01,0x00,0x00});
    else bytes({0x48,0x8b,0x81,0xb8,0x01,0x00,0x00});
    bytes({0x48,0x85,0xc0});
    normal.push_back(branch(0x84));
    bytes({0x8b,0x00}); // configuration first DWORD: interned class identifier
    for(std::size_t i=0;i<classes.size();++i){
        std::vector<std::size_t> matches;
        for(auto id:classes[i].class_id_rvas){bytes({0x48,0xb9});u64(base+id);bytes({0x3b,0x01});matches.push_back(branch(0x84));}
        bytes({0xe9});auto next=code.size();u32(0);
        for(auto p:matches)fix(p,code.size());
        bytes({0x48,0xb9});u64(controls+i);bytes({0x80,0x39,0x00});selected.push_back(branch(0x85));
        fix(next,code.size());
    }
    auto ordinary=code.size();
    bytes({0x5a,0x59,0x58,0x9d});
    code.insert(code.end(),site.original.begin(),site.original.end());
    bytes({0xff,0x25,0x00,0x00,0x00,0x00});u64(base+site.rva+site.original.size());
    auto enabled=code.size();
    bytes({0x5a,0x59,0x58,0x9d});
    if(site.kind==NoReloadSiteKind::Shared) bytes({0x49,0x8b,0x06});
    else if(site.kind==NoReloadSiteKind::DirectCallback) bytes({0x8b,0xd0,0xff,0x93,0x60,0x13,0x00,0x00});
    else bytes({0x8b,0xd0,0x48,0x8b,0x46,0xf8});
    bytes({0xff,0x25,0x00,0x00,0x00,0x00});u64(base+site.rva+site.original.size());
    for(auto p:normal)fix(p,ordinary);
    for(auto p:selected)fix(p,enabled);
    auto distance=static_cast<std::int64_t>(cave)-static_cast<std::int64_t>(base+site.rva+5);
    if(distance<std::numeric_limits<std::int32_t>::min() || distance>std::numeric_limits<std::int32_t>::max())throw std::runtime_error("No-reload cave is outside relative jump range");
    std::vector<std::uint8_t> jump{0xe9};
    for(unsigned i=0;i<4;++i)jump.push_back(static_cast<std::uint8_t>(static_cast<std::uint32_t>(distance)>>(8*i)));
    jump.resize(site.original.size(),0x90);
    return {code,{base+site.rva,site.original,jump}};
}
}
