#include "metro/core.hpp"
#include "metro/build_profile.hpp"
#include "metro/no_reload.hpp"
#include "metro/controls.hpp"
#include "metro/conditional_hooks.hpp"
#include <algorithm>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>

// Optionally check signatures against a locally captured original .text section.
// Game bytes are never required for the normal source-only test suite.
int main(int argc, char** argv) {
    try {
        std::map<std::string, bool> selection{{"no_reload", true}, {"god_mode", true}};
        if (metro::effective_controls(false, selection) != 0 ||
            metro::effective_controls(true, selection) != (metro::no_reload_control_bits | (1u << 5)))
            throw std::runtime_error("Global or paused control mask incorrect");
        selection["no_reload"] = false;
        const auto classes = metro::verified_no_reload_classes();
        for (unsigned i = 0; i < classes.size(); ++i) {
            selection[classes[i].feature] = true;
            if (metro::effective_controls(true, selection) != ((1u << (7+i)) | (1u << 5)))
                throw std::runtime_error("Individual control bit incorrect");
            selection[classes[i].feature] = false;
        }
        constexpr const char* ordinary[] = {"unlimited_ammo", "unlimited_throwables", "unlimited_medkits",
            "unlimited_filters", "indestructible_mask", "god_mode", "unlimited_flashlight"};
        selection.clear();
        for (unsigned i=0;i<7;++i) {
            selection[ordinary[i]]=true;
            if (metro::effective_controls(true, selection)!=(1u<<i)) throw std::runtime_error("Ordinary control bit incorrect");
            if (metro::effective_controls(false, selection)!=0) throw std::runtime_error("Paused ordinary control not zero");
            selection[ordinary[i]]=false;
        }
        selection["no_reload"]=true;
        const auto global=metro::effective_controls(true, selection);
        selection[classes.front().feature]=true;
        if(metro::effective_controls(true, selection)!=global) throw std::runtime_error("Shadowed selection changed effective controls");
        const auto remembered = selection;
        (void)metro::effective_controls(false, selection);
        if (selection != remembered) throw std::runtime_error("Pause mutated selections");
        std::vector<metro::CodePatchDefinition> definitions;
        for (const auto& site : metro::verified_conditional_sites()) {
            const auto image = metro::make_conditional_image(0x140000000, 0x141000000, 0x142000000, site);
            definitions.push_back({site.feature, site.rva, site.original, site.context_rva, site.context, image.hook.replacement});
        }
        for (const auto& site : metro::verified_no_reload_sites()) {
            auto replacement = site.original;
            replacement[0] = 0xe9; // The final rel32 depends on the runtime allocation.
            definitions.push_back({"no_reload", site.rva, site.original,
                                   site.rva, site.context, replacement});
        }
        std::vector<metro::Patch> patches;
        std::vector<std::uint8_t> text;
        if (argc == 2) {
            std::ifstream input(argv[1], std::ios::binary);
            if (!input) throw std::runtime_error("Cannot open captured .text");
            text.assign(std::istreambuf_iterator<char>(input), {});
            if (text.empty()) throw std::runtime_error("Empty captured .text");
        } else if (argc != 1) throw std::runtime_error("Usage: profile_tests [original-text.bin]");
        for (const auto& d : definitions) {
            const auto context = metro::parse_pattern(d.context);
            if (context.size() < 16 || d.original.empty() || d.original.size() != d.replacement.size()
                || d.original == d.replacement || !d.feature || !*d.feature)
                throw std::runtime_error("Invalid patch definition");
            if (d.rva < d.context_rva || d.rva - d.context_rva > context.size()
                || d.original.size() > context.size() - (d.rva - d.context_rva))
                throw std::runtime_error("Patch bytes lie outside signature context");
            for (std::size_t i = 0; i < d.original.size(); ++i) {
                const int expected = context[d.rva - d.context_rva + i];
                if (expected >= 0 && expected != d.original[i])
                    throw std::runtime_error("Signature disagrees with original bytes");
            }
            patches.push_back({d.rva, d.original, d.replacement});
            if (!text.empty()) {
                const auto matches = metro::scan_pattern(text, context);
                if (matches.size() != 1 || matches.front() + 0x1000 != d.context_rva)
                    throw std::runtime_error(std::string("Nonunique/misplaced signature: ") + d.feature);
                if (d.rva < 0x1000 || d.rva - 0x1000 > text.size()
                    || d.original.size() > text.size() - (d.rva - 0x1000)
                    || !std::equal(d.original.begin(), d.original.end(), text.begin() + d.rva - 0x1000))
                    throw std::runtime_error(std::string("Captured bytes disagree: ") + d.feature);
            }
        }
        // Constructor verifies disjoint patch ranges before any writes are possible.
        metro::PatchTransaction checked(patches);
        std::cout << definitions.size() << " patch profiles validated"
                  << (text.empty() ? ".\n" : " against captured original code.\n");
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
