//! Theme model: a small palette of ANSI/truecolor values with light and dark
//! variants, auto-detected from the terminal background (OSC 11), the
//! `COLORFGBG` environment variable, or the config file. Custom JSON themes
//! live in `~/.dorean/themes/<name>.json` and override the dark base palette.

use std::path::PathBuf;

use crossterm::style::Color;
use serde::Deserialize;

use crate::config::Config;

/// The config value for "follow the terminal's detected background".
pub const THEME_AUTO: &str = "auto";

/// The full palette used by the TUI. Every value is a crossterm [`Color`], so
/// themes can mix ANSI-256 and truecolor entries.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    /// Muted foreground for metadata, hints, dimmed text.
    pub dim: Color,
    /// Accent used for the header, focus highlights, and links.
    pub accent: Color,
    pub header_bg: Color,
    pub status_bg: Color,
    pub border: Color,
    pub selection_bg: Color,
    /// User message foreground (bubble).
    pub user: Color,
    /// Sub-agent name foreground.
    pub agent: Color,
    /// Tinted background panel behind user messages (opencode-style).
    pub user_bg: Color,
    /// Tinted background panel behind sub-agent messages.
    pub agent_bg: Color,
    /// Tinted background panel behind tool-call cards.
    pub tool_bg: Color,
    /// Muted gray left border bar for tool cards.
    pub tool_bar: Color,
    /// Tool name foreground on tool cards.
    pub tool_header: Color,
    pub code_bg: Color,
    pub code_fg: Color,
    pub error: Color,
    pub warning: Color,
    pub success: Color,
    /// Syntax highlight tokens.
    pub keyword: Color,
    pub string: Color,
    pub comment: Color,
    pub number: Color,
    pub type_color: Color,
    /// Diff colors.
    pub diff_add: Color,
    pub diff_del: Color,
    pub diff_hunk: Color,
}

impl Theme {
    /// Dark theme (default).
    pub fn dark() -> Self {
        Theme {
            bg: Color::Rgb {
                r: 0x18,
                g: 0x1a,
                b: 0x1e,
            },
            fg: Color::Rgb {
                r: 0xd4,
                g: 0xd6,
                b: 0xd8,
            },
            dim: Color::Rgb {
                r: 0x6b,
                g: 0x70,
                b: 0x78,
            },
            accent: Color::Rgb {
                r: 0x8a,
                g: 0xb4,
                b: 0xf8,
            },
            header_bg: Color::Rgb {
                r: 0x22,
                g: 0x25,
                b: 0x2b,
            },
            status_bg: Color::Rgb {
                r: 0x22,
                g: 0x25,
                b: 0x2b,
            },
            border: Color::Rgb {
                r: 0x3b,
                g: 0x40,
                b: 0x48,
            },
            selection_bg: Color::Rgb {
                r: 0x3b,
                g: 0x40,
                b: 0x48,
            },
            user: Color::Rgb {
                r: 0x89,
                g: 0xb4,
                b: 0xfe,
            },
            agent: Color::Rgb {
                r: 0xa6,
                g: 0xe3,
                b: 0xa1,
            },
            user_bg: Color::Rgb {
                r: 0x20,
                g: 0x27,
                b: 0x38,
            },
            agent_bg: Color::Rgb {
                r: 0x1f,
                g: 0x2b,
                b: 0x24,
            },
            tool_bg: Color::Rgb {
                r: 0x22,
                g: 0x25,
                b: 0x2b,
            },
            tool_bar: Color::Rgb {
                r: 0x4a,
                g: 0x4f,
                b: 0x57,
            },
            tool_header: Color::Rgb {
                r: 0xb5,
                g: 0xba,
                b: 0xbf,
            },
            code_bg: Color::Rgb {
                r: 0x10,
                g: 0x12,
                b: 0x15,
            },
            code_fg: Color::Rgb {
                r: 0xd4,
                g: 0xd6,
                b: 0xd8,
            },
            error: Color::Rgb {
                r: 0xf3,
                g: 0x8b,
                b: 0x8b,
            },
            warning: Color::Rgb {
                r: 0xf9,
                g: 0xd6,
                b: 0x69,
            },
            success: Color::Rgb {
                r: 0xa6,
                g: 0xe3,
                b: 0xa1,
            },
            keyword: Color::Rgb {
                r: 0xc7,
                g: 0x8b,
                b: 0xf5,
            },
            string: Color::Rgb {
                r: 0xa6,
                g: 0xe3,
                b: 0xa1,
            },
            comment: Color::Rgb {
                r: 0x6b,
                g: 0x70,
                b: 0x78,
            },
            number: Color::Rgb {
                r: 0xfa,
                g: 0xb3,
                b: 0x87,
            },
            type_color: Color::Rgb {
                r: 0x8a,
                g: 0xd6,
                b: 0xff,
            },
            diff_add: Color::Rgb {
                r: 0x40,
                g: 0x8a,
                b: 0x4e,
            },
            diff_del: Color::Rgb {
                r: 0xa8,
                g: 0x42,
                b: 0x42,
            },
            diff_hunk: Color::Rgb {
                r: 0x58,
                g: 0x75,
                b: 0x9a,
            },
        }
    }

    /// Light theme.
    pub fn light() -> Self {
        Theme {
            bg: Color::Rgb {
                r: 0xfa,
                g: 0xfa,
                b: 0xf8,
            },
            fg: Color::Rgb {
                r: 0x24,
                g: 0x29,
                b: 0x29,
            },
            dim: Color::Rgb {
                r: 0x7c,
                g: 0x81,
                b: 0x85,
            },
            accent: Color::Rgb {
                r: 0x1f,
                g: 0x5c,
                b: 0xb3,
            },
            header_bg: Color::Rgb {
                r: 0xec,
                g: 0xec,
                b: 0xe8,
            },
            status_bg: Color::Rgb {
                r: 0xec,
                g: 0xec,
                b: 0xe8,
            },
            border: Color::Rgb {
                r: 0xc9,
                g: 0xcb,
                b: 0xc9,
            },
            selection_bg: Color::Rgb {
                r: 0xd4,
                g: 0xe0,
                b: 0xf0,
            },
            user: Color::Rgb {
                r: 0x1f,
                g: 0x5c,
                b: 0xb3,
            },
            agent: Color::Rgb {
                r: 0x2e,
                g: 0x7d,
                b: 0x32,
            },
            user_bg: Color::Rgb {
                r: 0xdd,
                g: 0xe7,
                b: 0xf7,
            },
            agent_bg: Color::Rgb {
                r: 0xe0,
                g: 0xef,
                b: 0xe0,
            },
            tool_bg: Color::Rgb {
                r: 0xec,
                g: 0xec,
                b: 0xe8,
            },
            tool_bar: Color::Rgb {
                r: 0xb0,
                g: 0xb3,
                b: 0xb0,
            },
            tool_header: Color::Rgb {
                r: 0x44,
                g: 0x4a,
                b: 0x44,
            },
            code_bg: Color::Rgb {
                r: 0xf0,
                g: 0xf0,
                b: 0xec,
            },
            code_fg: Color::Rgb {
                r: 0x24,
                g: 0x29,
                b: 0x29,
            },
            error: Color::Rgb {
                r: 0xb3,
                g: 0x26,
                b: 0x1e,
            },
            warning: Color::Rgb {
                r: 0x8a,
                g: 0x64,
                b: 0x06,
            },
            success: Color::Rgb {
                r: 0x2e,
                g: 0x7d,
                b: 0x32,
            },
            keyword: Color::Rgb {
                r: 0x8f,
                g: 0x3f,
                b: 0xa8,
            },
            string: Color::Rgb {
                r: 0x2e,
                g: 0x7d,
                b: 0x32,
            },
            comment: Color::Rgb {
                r: 0x7c,
                g: 0x81,
                b: 0x85,
            },
            number: Color::Rgb {
                r: 0xc2,
                g: 0x4a,
                b: 0x66,
            },
            type_color: Color::Rgb {
                r: 0x00,
                g: 0x60,
                b: 0x8f,
            },
            diff_add: Color::Rgb {
                r: 0xa6,
                g: 0xd3,
                b: 0x9c,
            },
            diff_del: Color::Rgb {
                r: 0xf0,
                g: 0xa3,
                b: 0x9c,
            },
            diff_hunk: Color::Rgb {
                r: 0xb3,
                g: 0xc9,
                b: 0xe0,
            },
        }
    }

    /// The theme for a run: an explicit config override wins, then the
    /// terminal's detected background (OSC 11, best-effort), then the
    /// `COLORFGBG` environment heuristic, then dark.
    ///
    /// `config.theme` accepts `auto` (the default — detect), `light`, `dark`,
    /// or the name of a JSON theme in `~/.dorean/themes/`. A custom theme that
    /// fails to load falls back to detection.
    pub fn detect(config: &Config) -> Theme {
        match config
            .theme
            .as_deref()
            .map(|t| t.trim().to_ascii_lowercase())
        {
            Some(name) if name == "light" => Theme::light(),
            Some(name) if name == "dark" => Theme::dark(),
            Some(name) if name != THEME_AUTO && !name.is_empty() => {
                Theme::load_custom(&name).unwrap_or_else(detected)
            }
            _ => detected(),
        }
    }

    /// Where custom themes are looked up: `$DOREAN_THEME_DIR`, else
    /// `~/.dorean/themes`.
    pub fn themes_dir() -> PathBuf {
        std::env::var("DOREAN_THEME_DIR")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                crate::config::Config::config_dir()
                    .unwrap_or_else(|_| PathBuf::from(".dorean"))
                    .join("themes")
            })
    }

    /// Custom themes live in `$DOREAN_THEME_DIR/<name>.json` (default
    /// `~/.dorean/themes/`). Returns the parsed theme (dark base + overrides)
    /// or `None` when the file is missing or malformed.
    pub fn load_custom(name: &str) -> Option<Theme> {
        let text = std::fs::read_to_string(Self::themes_dir().join(format!("{name}.json"))).ok()?;
        let patch: ThemePatch = serde_json::from_str(&text).ok()?;
        let mut theme = Theme::dark();
        patch.apply(&mut theme);
        Some(theme)
    }

    /// The names of all custom JSON themes installed (file stem of each
    /// `*.json`, sorted).
    pub fn custom_theme_names() -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(Self::themes_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Query the live terminal background (OSC 11). Best-effort; falls back to
    /// `None` so callers can use a default theme. Should be called right after
    /// [`crate::tui::terminal::Terminal::enter`].
    pub fn detect_from_terminal() -> Option<Theme> {
        let rgb = crate::tui::terminal::query_background_color(150)?;
        // Perceived luminance: light backgrounds (> 128) get the light theme.
        let luminance = (rgb.0 as u16 * 299 + rgb.1 as u16 * 587 + rgb.2 as u16 * 114) / 1000;
        Some(if luminance > 128 {
            Theme::light()
        } else {
            Theme::dark()
        })
    }
}

/// A JSON theme file: every field optional, all values CSS-style `#rrggbb`
/// hex or ANSI color names. Missing fields inherit from the dark base.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ThemePatch {
    bg: Option<String>,
    fg: Option<String>,
    dim: Option<String>,
    accent: Option<String>,
    header_bg: Option<String>,
    status_bg: Option<String>,
    border: Option<String>,
    selection_bg: Option<String>,
    user: Option<String>,
    agent: Option<String>,
    user_bg: Option<String>,
    agent_bg: Option<String>,
    tool_bg: Option<String>,
    tool_bar: Option<String>,
    tool_header: Option<String>,
    code_bg: Option<String>,
    code_fg: Option<String>,
    error: Option<String>,
    warning: Option<String>,
    success: Option<String>,
    keyword: Option<String>,
    string: Option<String>,
    comment: Option<String>,
    number: Option<String>,
    type_color: Option<String>,
    diff_add: Option<String>,
    diff_del: Option<String>,
    diff_hunk: Option<String>,
}

impl ThemePatch {
    fn apply(&self, theme: &mut Theme) {
        macro_rules! patch {
            ($($field:ident),* $(,)?) => {
                $(
                    if let Some(value) = &self.$field
                        && let Some(color) = parse_color(value)
                    {
                        theme.$field = color;
                    }
                )*
            };
        }
        patch!(
            bg,
            fg,
            dim,
            accent,
            header_bg,
            status_bg,
            border,
            selection_bg,
            user,
            agent,
            user_bg,
            agent_bg,
            tool_bg,
            tool_bar,
            tool_header,
            code_bg,
            code_fg,
            error,
            warning,
            success,
            keyword,
            string,
            comment,
            number,
            type_color,
            diff_add,
            diff_del,
            diff_hunk,
        );
    }
}

/// Parse a `#rrggbb` hex string or a basic ANSI color name.
fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        return Some(Color::Rgb { r, g, b });
    }
    match value.to_ascii_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::DarkRed),
        "green" => Some(Color::DarkGreen),
        "yellow" => Some(Color::DarkYellow),
        "blue" => Some(Color::DarkBlue),
        "magenta" => Some(Color::DarkMagenta),
        "cyan" => Some(Color::DarkCyan),
        "white" => Some(Color::Grey),
        "gray" | "grey" => Some(Color::DarkGrey),
        "bright-red" => Some(Color::Red),
        "bright-green" => Some(Color::Green),
        "bright-yellow" => Some(Color::Yellow),
        "bright-blue" => Some(Color::Blue),
        "bright-magenta" => Some(Color::Magenta),
        "bright-cyan" => Some(Color::Cyan),
        "bright-white" => Some(Color::White),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    Light,
    Dark,
}

fn detected() -> Theme {
    match detected_choice() {
        ThemeChoice::Light => Theme::light(),
        ThemeChoice::Dark => Theme::dark(),
    }
}

fn detected_choice() -> ThemeChoice {
    if let Ok(value) = std::env::var("COLORFGBG") {
        // Format: "<fg>;<bg>" where the second field is the background.
        if let Some(bg) = value.split(';').nth(1)
            && let Ok(bg) = bg.parse::<u8>()
        {
            return if bg >= 8 {
                ThemeChoice::Light
            } else {
                ThemeChoice::Dark
            };
        }
    }
    ThemeChoice::Dark
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[serial]
    #[test]
    fn explicit_theme_wins() {
        let config = Config {
            theme: Some("light".to_string()),
            ..Config::default()
        };
        assert_eq!(Theme::detect(&config).bg, Theme::light().bg);

        let config = Config {
            theme: Some("dark".to_string()),
            ..Config::default()
        };
        assert_eq!(Theme::detect(&config).bg, Theme::dark().bg);
    }

    #[test]
    fn unknown_theme_falls_back() {
        let config = Config {
            theme: Some("solarized".to_string()),
            ..Config::default()
        };
        // Must return a fully-formed theme (dark default here).
        let _ = Theme::detect(&config);
    }

    #[serial]
    #[test]
    fn colorfgbg_detection() {
        unsafe {
            std::env::set_var("COLORFGBG", "15;0"); // dark bg
        }
        assert_eq!(detected_choice(), ThemeChoice::Dark);
        unsafe {
            std::env::set_var("COLORFGBG", "0;15"); // light bg
        }
        assert_eq!(detected_choice(), ThemeChoice::Light);
    }

    #[serial]
    #[test]
    fn auto_theme_means_detection() {
        let config = Config {
            theme: Some("auto".to_string()),
            ..Config::default()
        };
        unsafe {
            std::env::set_var("COLORFGBG", "0;15"); // light terminal
        }
        assert_eq!(Theme::detect(&config).bg, Theme::light().bg);
    }

    #[test]
    fn parses_hex_and_named_colors() {
        assert_eq!(
            parse_color("#ff0000"),
            Some(Color::Rgb {
                r: 0xff,
                g: 0x00,
                b: 0x00
            })
        );
        assert_eq!(parse_color("bright-green"), Some(Color::Green));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("neon"), None);
    }

    #[serial]
    #[test]
    fn custom_theme_overrides_dark_base() {
        unsafe {
            std::env::set_var("DOREAN_THEME_DIR", theme_test_dir());
        }
        std::fs::create_dir_all(Theme::themes_dir()).unwrap();
        let path = Theme::themes_dir().join("solarized.json");
        std::fs::write(
            &path,
            r##"{"bg": "#002b36", "fg": "#839496", "accent": "bright-blue"}"##,
        )
        .unwrap();

        let theme = Theme::load_custom("solarized").unwrap();
        assert_eq!(
            theme.bg,
            Color::Rgb {
                r: 0x00,
                g: 0x2b,
                b: 0x36
            }
        );
        assert_eq!(
            theme.fg,
            Color::Rgb {
                r: 0x83,
                g: 0x94,
                b: 0x96
            }
        );
        assert_eq!(theme.accent, Color::Blue);
        // Unset fields inherit the dark base.
        assert_eq!(theme.code_bg, Theme::dark().code_bg);

        // Config routing picks the custom theme up.
        let config = Config {
            theme: Some("solarized".to_string()),
            ..Config::default()
        };
        assert_eq!(Theme::detect(&config).bg, theme.bg);

        assert!(Theme::custom_theme_names().contains(&"solarized".to_string()));
    }

    fn theme_test_dir() -> String {
        std::env::temp_dir()
            .join(format!("dorean-themes-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[serial]
    #[test]
    fn missing_custom_theme_falls_back_to_detection() {
        unsafe {
            std::env::set_var("COLORFGBG", "15;0"); // dark terminal
        }
        let config = Config {
            theme: Some("no-such-theme".to_string()),
            ..Config::default()
        };
        // Must not panic; returns the detected (dark default) theme.
        assert_eq!(Theme::detect(&config).bg, Theme::dark().bg);
    }
}
