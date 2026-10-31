#include "metro/core.hpp"
#include "metro/build_profile.hpp"
#include "metro/no_reload.hpp"
#include <algorithm>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>

// Optionally check signatures against a locally captured original .text section.
// Game bytes are never required for the normal source-only test suite.
int main(int argc, char** argv) {
    try {
        auto definitions = metro::verified_code_patches();
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
