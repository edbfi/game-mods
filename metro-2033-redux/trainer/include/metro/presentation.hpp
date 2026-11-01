// SPDX-License-Identifier: AGPL-3.0-only
#pragma once
#include "metro/core.hpp"
#include <map>
#include <string>
#include <vector>

namespace metro::ui {
enum class Lifecycle { Waiting, Preparing, Installing, Attached, Restoring, Stopped, Error };
enum class Tone { Normal, Amber, Muted, Error };
enum class Navigation { Previous, Next };
struct Model {
    Config config;
    // Runtime selection and successfully applied state, not requested INI values.
    std::map<std::string, bool> selected;
    std::map<std::string, bool> effective;
    bool master = false;
    Lifecycle lifecycle = Lifecycle::Waiting;
    std::string logfile;
    std::string status = "Waiting for the game.";
    // Persistent until explicitly cleared by the runtime; include recovery action.
    std::string error;
};
struct Row {
    std::string text;
    Tone tone = Tone::Normal;
    bool operator==(const Row& other) const { return text == other.text && tone == other.tone; }
};
struct Frame {
    std::vector<Row> rows;
    std::size_t width = 0;
    std::size_t pages = 1;
    std::size_t page = 0;
    bool dashboard = false;
};
struct DirtyRows { std::size_t first; std::vector<Row> rows; };
void acknowledge(Model& model, bool master, const std::map<std::string, bool>& selected,
                 std::uint32_t controls);
std::string binding_label(unsigned binding);
std::string effective_label(const Model& model, const std::string& feature);
// Small/plain frames wrap all entries rather than silently hiding features.
Frame render(const Model& model, std::size_t width, std::size_t height,
             std::size_t weapon_page = 0, bool interactive = true);
// Rows are space-padded, including vanished rows: writing these cannot leave tails.
std::vector<DirtyRows> diff(const Frame& previous, const Frame& next, bool invalidate = false);
std::string plain_text(const Frame& frame);
}
