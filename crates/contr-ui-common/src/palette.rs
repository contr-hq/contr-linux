//! Contr's colours for what code draws: the Stats charts, Compose service names and log highlights.
//! `data/style.css` holds the same palette for everything CSS draws.

use relm4::gtk::gdk;

/// Service names in Compose logs, in service order; they repeat after the last. The console is dark in both schemes.
pub const SERVICE_COLORS: [&str; 6] = ["#94cf9f", "#e0ce89", "#98c3e8", "#e59b94", "#cfa7d8", "#e9ba8d"];

/// Timestamps and notes in the logs console.
pub const CONSOLE_DIM: &str = "#8e9398";

/// A search hit in the logs or inspect console.
pub const MATCH_BACKGROUND: &str = "#e0c16d";
pub const MATCH_FOREGROUND: &str = "#161a1e";

/// The background of a log line that holds a search hit.
pub const MATCH_LINE_BACKGROUND: &str = "#3a211f";

/// The text and background of the console, as `.console` draws them in both colour schemes.
#[derive(Clone, Copy, Debug)]
pub struct ConsoleColors {
    pub foreground: gdk::RGBA,
    pub background: gdk::RGBA,
}

pub fn console() -> ConsoleColors {
    ConsoleColors {
        foreground: rgb(0xd8, 0xdb, 0xdf),
        background: rgb(0x16, 0x1a, 0x1e),
    }
}

/// The colours of a chart in the current colour scheme.
#[derive(Clone, Copy, Debug)]
pub struct ChartColors {
    /// The first series, in the accent as `--accent-color` resolves it and `.legend-received` draws it.
    pub primary: gdk::RGBA,
    /// The second series, as `.legend-sent` draws it.
    pub secondary: gdk::RGBA,
    /// A limit line, in the error colour.
    pub limit: gdk::RGBA,
}

pub fn chart(dark: bool) -> ChartColors {
    if dark {
        ChartColors {
            primary: rgb(0xae, 0xd3, 0xef),
            secondary: rgb(0xc1, 0xa3, 0xd8),
            limit: rgb(0xff, 0xab, 0xa3),
        }
    } else {
        ChartColors {
            primary: rgb(0x46, 0x68, 0x80),
            secondary: rgb(0x85, 0x65, 0x9c),
            limit: rgb(0xa4, 0x3b, 0x38),
        }
    }
}

fn rgb(red: u8, green: u8, blue: u8) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        1.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: &str = include_str!("../data/style.css");

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn hex(color: gdk::RGBA) -> String {
        let channel = |value: f32| (value * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(color.red()),
            channel(color.green()),
            channel(color.blue())
        )
    }

    #[test]
    fn light_charts_draw_the_stylesheet_accent() {
        let accent = format!("--accent-bg-color: {};", hex(chart(false).primary));
        assert!(STYLE.contains(&accent), "style.css has no `{accent}`");
    }

    #[test]
    fn console_matches_the_stylesheet() {
        let console = console();
        let rule = format!(
            ".console {{\n  background-color: {};\n  color: {};",
            hex(console.background),
            hex(console.foreground)
        );
        assert!(STYLE.contains(&rule), "style.css has no `{rule}`");
    }

    #[test]
    fn series_match_their_legends_in_both_schemes() {
        for dark in [false, true] {
            let colors = chart(dark);
            for (class, color) in [("legend-received", colors.primary), ("legend-sent", colors.secondary)] {
                let legend = format!(".{class} {{ background-color: {}; }}", hex(color));
                assert!(STYLE.contains(&legend), "style.css has no `{legend}`");
            }
        }
    }
}
