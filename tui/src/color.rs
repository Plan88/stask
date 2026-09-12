use ratatui::style::Color;

/// Maps a status color name to a terminal color. Unknown names fall back to
/// the terminal default instead of erroring, so a bad name stored in the
/// database degrades display quality but never blocks startup.
pub fn color_from_name(name: &str) -> Color {
    match name {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" => Color::Gray,
        "dark_gray" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests the mapping of the standard color names.
    // Given: each color name accepted for statuses
    // When: it is mapped to a terminal color
    // Then: it yields the matching ANSI color, including both grays and the
    //       light_* variants
    #[test]
    fn known_names_map_to_ansi_colors() {
        let cases = [
            ("black", Color::Black),
            ("red", Color::Red),
            ("green", Color::Green),
            ("yellow", Color::Yellow),
            ("blue", Color::Blue),
            ("magenta", Color::Magenta),
            ("cyan", Color::Cyan),
            ("white", Color::White),
            ("gray", Color::Gray),
            ("dark_gray", Color::DarkGray),
            ("light_red", Color::LightRed),
            ("light_green", Color::LightGreen),
            ("light_yellow", Color::LightYellow),
            ("light_blue", Color::LightBlue),
            ("light_magenta", Color::LightMagenta),
            ("light_cyan", Color::LightCyan),
        ];

        for (name, expected) in cases {
            assert_eq!(color_from_name(name), expected, "for name `{name}`");
        }
    }

    // Tests the fallback for unknown color names.
    // Given: a name no palette defines (e.g. a typo like "grean")
    // When: it is mapped to a terminal color
    // Then: it yields the terminal default color rather than failing
    #[test]
    fn unknown_name_falls_back_to_terminal_default() {
        assert_eq!(color_from_name("grean"), Color::Reset);
        assert_eq!(color_from_name(""), Color::Reset);
    }
}
