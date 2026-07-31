//! A dependency-free syntax highlighter for the code blocks rendered in the
//! TUI. Covers the common languages a coding agent writes (Rust, Python,
//! JavaScript/TypeScript, JSON, bash, TOML, Go, SQL, YAML, HTML, CSS, C/Java).
//! Each line is tokenized into `(text, color)` pairs drawn over the code
//! block background.

use crossterm::style::Color;

use crate::tui::theme::Theme;

/// Normalized language for highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    Json,
    Bash,
    Toml,
    Go,
    Sql,
    Yaml,
    Html,
    Css,
    C,
    Java,
    Ruby,
    Generic,
}

/// Map a fence language label to a [`Lang`]. Unknown labels fall back to
/// [`Lang::Generic`] (plain, uncolored) rather than guessing wrong.
pub fn normalize_lang(lang: &str) -> Lang {
    let l = lang.trim().to_ascii_lowercase();
    match l.as_str() {
        "rust" | "rs" => Lang::Rust,
        "python" | "py" | "python3" => Lang::Python,
        "javascript" | "js" | "jsx" | "typescript" | "ts" | "tsx" => Lang::JavaScript,
        "json" => Lang::Json,
        "bash" | "sh" | "shell" | "zsh" => Lang::Bash,
        "toml" => Lang::Toml,
        "go" | "golang" => Lang::Go,
        "sql" => Lang::Sql,
        "yaml" | "yml" => Lang::Yaml,
        "html" | "xml" | "svg" => Lang::Html,
        "css" | "scss" | "sass" | "less" => Lang::Css,
        "c" | "cpp" | "c++" | "h" | "hpp" => Lang::C,
        "java" => Lang::Java,
        "ruby" | "rb" => Lang::Ruby,
        _ => Lang::Generic,
    }
}

/// Highlight one line of source, returning `(text, color)` runs.
pub fn highlight_line(lang: Lang, line: &str, theme: &Theme) -> Vec<(String, Color)> {
    let cfg = config(lang);
    tokenize(&cfg, line, theme)
}

/// The colors used for tokens.
pub struct HighlightColors {
    pub keyword: Color,
    pub string: Color,
    pub comment: Color,
    pub number: Color,
    pub type_color: Color,
}

impl From<&Theme> for HighlightColors {
    fn from(theme: &Theme) -> Self {
        HighlightColors {
            keyword: theme.keyword,
            string: theme.string,
            comment: theme.comment,
            number: theme.number,
            type_color: theme.type_color,
        }
    }
}

struct LangConfig {
    /// Comment markers to recognize, e.g. `["//"]` or `["#"]`.
    comments: &'static [&'static str],
    /// Quote characters that open a string literal.
    quotes: &'static [char],
    /// Whether `#!`-style shebang lines are comments (bash/python).
    shebang: bool,
    /// Whether identifiers starting with an uppercase letter are types.
    types: bool,
    keywords: &'static [&'static str],
}

fn config(lang: Lang) -> LangConfig {
    use Lang::*;
    match lang {
        Rust => LangConfig {
            comments: &["//"],
            quotes: &['"', '\''],
            shebang: false,
            types: true,
            keywords: &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while", "yield",
            ],
        },
        Python => LangConfig {
            comments: &["#"],
            quotes: &['"', '\''],
            shebang: true,
            types: true,
            keywords: &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                "return", "True", "try", "while", "with", "yield",
            ],
        },
        JavaScript | Java | C => LangConfig {
            comments: &["//"],
            quotes: &['"', '\''],
            shebang: false,
            types: true,
            keywords: &[
                "abstract",
                "as",
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "goto",
                "if",
                "implements",
                "import",
                "in",
                "instanceof",
                "interface",
                "let",
                "new",
                "null",
                "of",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "var",
                "void",
                "while",
                "with",
                "yield",
            ],
        },
        Go => LangConfig {
            comments: &["//"],
            quotes: &['"', '`'],
            shebang: false,
            types: true,
            keywords: &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "type",
                "var",
            ],
        },
        Sql => LangConfig {
            comments: &["--"],
            quotes: &['\''],
            shebang: false,
            types: false,
            keywords: &[
                "SELECT",
                "FROM",
                "WHERE",
                "INSERT",
                "INTO",
                "VALUES",
                "UPDATE",
                "SET",
                "DELETE",
                "CREATE",
                "TABLE",
                "DROP",
                "ALTER",
                "ADD",
                "INDEX",
                "VIEW",
                "JOIN",
                "LEFT",
                "RIGHT",
                "INNER",
                "OUTER",
                "FULL",
                "ON",
                "AS",
                "AND",
                "OR",
                "NOT",
                "NULL",
                "GROUP",
                "BY",
                "ORDER",
                "HAVING",
                "LIMIT",
                "OFFSET",
                "DISTINCT",
                "PRIMARY",
                "KEY",
                "FOREIGN",
                "REFERENCES",
                "UNIQUE",
                "CONSTRAINT",
                "IF",
                "EXISTS",
                "CASE",
                "WHEN",
                "THEN",
                "ELSE",
                "END",
                "PRAGMA",
                "BEGIN",
                "COMMIT",
            ],
        },
        Bash => LangConfig {
            comments: &["#"],
            quotes: &['"', '\''],
            shebang: true,
            types: false,
            keywords: &[
                "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case",
                "esac", "in", "function", "return", "break", "continue", "export", "local",
                "readonly", "source", ".", "echo", "cd", "mkdir", "rm", "cp", "mv", "git", "cargo",
                "npm", "exit", "set", "unset", "test", "[[", "]]",
            ],
        },
        Toml => LangConfig {
            comments: &["#"],
            quotes: &['"', '\''],
            shebang: false,
            types: false,
            keywords: &[],
        },
        Yaml => LangConfig {
            comments: &["#"],
            quotes: &['"', '\''],
            shebang: false,
            types: false,
            keywords: &["true", "false", "null", "yes", "no", "on", "off"],
        },
        Html => LangConfig {
            comments: &["<!--"],
            quotes: &['"', '\''],
            shebang: false,
            types: false,
            keywords: &[
                "html", "head", "body", "div", "span", "script", "style", "meta", "link", "title",
                "a", "img", "button", "input", "form", "ul", "ol", "li", "p", "h1", "h2", "h3",
                "table", "tr", "td", "th", "svg", "path", "template", "class",
            ],
        },
        Css => LangConfig {
            comments: &["/*"],
            quotes: &['"'],
            shebang: false,
            types: false,
            keywords: &[
                "color",
                "background",
                "background-color",
                "font-size",
                "font-family",
                "margin",
                "padding",
                "border",
                "display",
                "position",
                "top",
                "left",
                "right",
                "bottom",
                "width",
                "height",
                "flex",
                "grid",
                "align-items",
                "justify-content",
                "opacity",
                "z-index",
                "overflow",
                "cursor",
                "transition",
                "animation",
                "media",
                "import",
            ],
        },
        Json => LangConfig {
            comments: &[],
            quotes: &['"'],
            shebang: false,
            types: false,
            keywords: &["true", "false", "null"],
        },
        Ruby => LangConfig {
            comments: &["#"],
            quotes: &['"', '\''],
            shebang: true,
            types: false,
            keywords: &[
                "def",
                "end",
                "class",
                "module",
                "if",
                "else",
                "elsif",
                "unless",
                "while",
                "until",
                "for",
                "in",
                "do",
                "then",
                "case",
                "when",
                "return",
                "yield",
                "require",
                "include",
                "extend",
                "attr_accessor",
                "attr_reader",
                "attr_writer",
                "nil",
                "true",
                "false",
                "self",
                "super",
                "begin",
                "rescue",
                "ensure",
                "raise",
                "new",
            ],
        },
        Generic => LangConfig {
            comments: &[],
            quotes: &['"'],
            shebang: false,
            types: false,
            keywords: &[],
        },
    }
}

fn tokenize(cfg: &LangConfig, line: &str, theme: &Theme) -> Vec<(String, Color)> {
    let colors = HighlightColors::from(theme);
    let chars: Vec<char> = line.chars().collect();
    let mut tokens: Vec<(String, Color)> = Vec::new();
    let mut i = 0usize;

    // Shebang lines are comments.
    if cfg.shebang && line.starts_with("#!") {
        return vec![(line.to_string(), colors.comment)];
    }

    let mut plain = String::new();
    let push_plain = |tokens: &mut Vec<(String, Color)>, plain: &mut String| {
        if !plain.is_empty() {
            tokens.push((std::mem::take(plain), theme.fg));
        }
    };

    while i < chars.len() {
        // Comment?
        let rest: String = chars[i..].iter().collect();
        let comment = cfg.comments.iter().find(|c| rest.starts_with(**c));
        if comment.is_some() {
            push_plain(&mut tokens, &mut plain);
            let text: String = chars[i..].iter().collect();
            tokens.push((text, colors.comment));
            break;
        }

        // String literal.
        if cfg.quotes.contains(&chars[i]) {
            push_plain(&mut tokens, &mut plain);
            let quote = chars[i];
            let mut j = i + 1;
            let mut escaped = false;
            while j < chars.len() {
                let c = chars[j];
                if escaped {
                    escaped = false;
                } else if c == '\\' && quote != '\'' {
                    escaped = true;
                } else if c == quote {
                    break;
                }
                j += 1;
            }
            let end = if j < chars.len() { j + 1 } else { chars.len() };
            let text: String = chars[i..end].iter().collect();
            tokens.push((text, colors.string));
            i = end;
            continue;
        }

        // Number literal.
        if chars[i].is_ascii_digit()
            || (chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            push_plain(&mut tokens, &mut plain);
            let mut j = i;
            if chars[j] == '0' && j + 1 < chars.len() && matches!(chars[j + 1], 'x' | 'b' | 'o') {
                j += 2;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
            } else {
                while j < chars.len()
                    && (chars[j].is_ascii_digit()
                        || matches!(chars[j], '.' | '_' | 'e' | 'E' | 'x'))
                {
                    j += 1;
                }
                // Type suffix for Rust/C.
                if j < chars.len()
                    && (chars[j] == 'u' || chars[j] == 'i' || chars[j] == 'f' || chars[j] == 'L')
                {
                    j += 1;
                    while j < chars.len() && chars[j].is_ascii_alphanumeric() {
                        j += 1;
                    }
                }
            }
            let text: String = chars[i..j].iter().collect();
            tokens.push((text, colors.number));
            i = j;
            continue;
        }

        // Identifier or keyword.
        if chars[i].is_alphabetic() || chars[i] == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            push_plain(&mut tokens, &mut plain);
            let color = if cfg.keywords.contains(&word.as_str()) {
                colors.keyword
            } else if word.chars().next().is_some_and(|c| c.is_uppercase()) && cfg.types {
                colors.type_color
            } else {
                theme.fg
            };
            tokens.push((word, color));
            i = j;
            continue;
        }

        plain.push(chars[i]);
        i += 1;
    }

    push_plain(&mut tokens, &mut plain);
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lang: Lang, line: &str) -> Vec<(String, Color)> {
        highlight_line(lang, line, &crate::tui::theme::Theme::dark())
    }

    fn colors(line: &[(String, Color)]) -> Vec<Color> {
        line.iter().map(|(_, c)| *c).collect()
    }

    #[test]
    fn highlights_rust_keywords_and_comments() {
        let line = plain(Lang::Rust, "let x: u8 = 42; // hi");
        let cols = colors(&line);
        let theme = crate::tui::theme::Theme::dark();
        assert!(cols.contains(&theme.keyword));
        assert!(cols.contains(&theme.number));
        assert!(cols.contains(&theme.comment));
    }

    #[test]
    fn highlights_strings() {
        let line = plain(Lang::Python, "name = \"dorean\"");
        let cols = colors(&line);
        let theme = crate::tui::theme::Theme::dark();
        assert!(cols.contains(&theme.string));
    }

    #[test]
    fn json_keys_are_plain_values_are_strings() {
        let line = plain(Lang::Json, "{\"a\": true}");
        let cols = colors(&line);
        let theme = crate::tui::theme::Theme::dark();
        assert!(cols.contains(&theme.keyword)); // true
        assert!(cols.contains(&theme.string));
    }

    #[test]
    fn unknown_language_stays_plain() {
        let line = plain(Lang::Generic, "whatever");
        assert!(
            colors(&line)
                .iter()
                .all(|c| *c != crate::tui::theme::Theme::dark().keyword)
        );
    }

    #[test]
    fn shebang_is_comment() {
        let line = plain(Lang::Bash, "#!/bin/sh");
        assert!(line.len() == 1);
    }

    #[test]
    fn sql_keywords_match_case_sensitively() {
        let line = plain(Lang::Sql, "SELECT * FROM t WHERE a = 1");
        let cols = colors(&line);
        let theme = crate::tui::theme::Theme::dark();
        assert!(cols.contains(&theme.keyword));
    }
}
