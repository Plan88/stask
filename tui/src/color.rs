use ratatui::style::Color;

/// Maps a status color spec to a terminal color. Unknown specs fall back to
/// the terminal default instead of erroring, so a bad spec stored in the
/// database degrades display quality but never blocks startup.
pub fn parse_color(name: &str) -> Color {
    if let Some(hex) = name.strip_prefix('#') {
        return rgb_from_hex(hex).unwrap_or(Color::Reset);
    }
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

/// Reads the `rrggbb` digits of a hex spec; None rejects anything but
/// exactly six hex digits. The ASCII check keeps the byte slicing below
/// away from multi-byte characters.
fn rgb_from_hex(hex: &str) -> Option<Color> {
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
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
            assert_eq!(parse_color(name), expected, "for name `{name}`");
        }
    }

    // Tests the fallback for unknown color names.
    // Given: a name no palette defines (e.g. a typo like "grean")
    // When: it is mapped to a terminal color
    // Then: it yields the terminal default color rather than failing
    #[test]
    fn unknown_name_falls_back_to_terminal_default() {
        assert_eq!(parse_color("grean"), Color::Reset);
        assert_eq!(parse_color(""), Color::Reset);
    }

    // Tests parsing of #rrggbb hex color specs.
    // Given: a mixed hex value, its uppercase spelling, and the black and
    //        white extremes
    // When: each is parsed
    // Then: each yields the matching RGB color, case-insensitively
    #[test]
    fn hex_spec_parses_to_rgb() {
        assert_eq!(parse_color("#ff8800"), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("#FF8800"), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color("#000000"), Color::Rgb(0, 0, 0));
        assert_eq!(parse_color("#ffffff"), Color::Rgb(255, 255, 255));
    }

    // Tests the fallback for malformed hex specs.
    // Given: hex specs with too few or too many digits, a non-hex digit,
    //        and six valid digits missing the leading #
    // When: each is parsed
    // Then: each yields the terminal default color, like an unknown name
    #[test]
    fn malformed_hex_spec_falls_back_to_terminal_default() {
        assert_eq!(parse_color("#12345"), Color::Reset);
        assert_eq!(parse_color("#1234567"), Color::Reset);
        assert_eq!(parse_color("#gg0000"), Color::Reset);
        assert_eq!(parse_color("ff8800"), Color::Reset);
    }
}
