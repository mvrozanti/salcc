//! Terminal capability detection, resolved once at startup from the
//! environment. Degrade chain: truecolor -> 256 -> 16 -> none. `NO_COLOR`,
//! an unset/dumb TERM, or (for the live proxy) a non-tty stdout disable
//! styling entirely — bit-perfect passthrough is the contract there.
//! Undercurl (SGR 4:3 + 58;2;r;g;b) is allowlisted by TERM/TERM_PROGRAM
//! because there is no reliable query; everything else degrades to plain
//! underline. Synchronized update (CSI ?2026, used to wrap repaint bursts)
//! is allowlisted the same way. Tests construct `Caps` directly instead of
//! mutating the environment.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    C256,
    C16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub styling_enabled: bool,
    pub depth: ColorDepth,
    pub undercurl: bool,
    pub sync_update: bool,
}

const UNDERCURL_TERMS: [&str; 6] = ["kitty", "wezterm", "foot", "alacritty", "konsole", "ghostty"];
const SYNC_UPDATE_TERMS: [&str; 6] = ["kitty", "wezterm", "foot", "alacritty", "ghostty", "contour"];

impl Caps {
    pub fn detect_for_proxy() -> Self {
        let tty = rustix::termios::isatty(rustix::stdio::stdout());
        Self::detect_with_tty(tty)
    }

    pub fn detect_for_replay() -> Self {
        Self::detect_with_tty(true)
    }

    fn detect_with_tty(stdout_is_tty: bool) -> Self {
        Self::from_env_parts(
            std::env::var("TERM").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var_os("NO_COLOR").is_some(),
            stdout_is_tty,
        )
    }

    pub fn from_env_parts(
        term: Option<&str>,
        colorterm: Option<&str>,
        term_program: Option<&str>,
        no_color: bool,
        stdout_is_tty: bool,
    ) -> Self {
        let term = term.unwrap_or("");
        let known_terminal = !term.is_empty() && term != "dumb";
        let styling_enabled = stdout_is_tty && known_terminal && !no_color;
        let colorterm = colorterm.unwrap_or("");
        let depth = if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            ColorDepth::TrueColor
        } else if term.contains("256color") {
            ColorDepth::C256
        } else {
            ColorDepth::C16
        };
        let program = term_program.unwrap_or("").to_ascii_lowercase();
        let undercurl = UNDERCURL_TERMS
            .iter()
            .any(|t| term.contains(t) || program.contains(t));
        let sync_update = SYNC_UPDATE_TERMS
            .iter()
            .any(|t| term.contains(t) || program.contains(t));
        let caps = Self {
            styling_enabled,
            depth,
            undercurl,
            sync_update,
        };
        tracing::info!(?caps, term, colorterm, program, "terminal capabilities detected");
        caps
    }

    pub fn disabled() -> Self {
        Self {
            styling_enabled: false,
            depth: ColorDepth::C16,
            undercurl: false,
            sync_update: false,
        }
    }

    pub fn truecolor_undercurl() -> Self {
        Self {
            styling_enabled: true,
            depth: ColorDepth::TrueColor,
            undercurl: true,
            sync_update: true,
        }
    }
}
