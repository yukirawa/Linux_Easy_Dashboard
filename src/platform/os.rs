//! freedesktop `os-release` reader: which distribution are we running on.
//!
//! Only the files are consulted, never the environment, so a dashboard started
//! from a sandbox still reports the machine's distribution. Parsing is a pure
//! function over the file contents; reading a missing file just yields `None`.

use log::debug;

/// Looked up in this order, as the specification recommends: `/etc/os-release`
/// is the administrator's override, and distributions ship the file in
/// `/usr/lib` (a symlink target in practice, so both usually hold the same
/// text).
const PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// Distribution information, from the freedesktop `os-release`.
#[derive(Debug, Clone, Default)]
pub struct Release {
    /// `ID` (e.g. `arch`).
    pub id: String,
    /// `NAME` (e.g. `Arch Linux`).
    pub name: String,
    /// `PRETTY_NAME`, falling back to `NAME` when absent.
    pub pretty_name: String,
    /// `VERSION_ID` or `BUILD_ID` (e.g. `rolling`).
    pub version: String,
    /// `HOME_URL`.
    pub home_url: String,
    /// `LOGO` (an icon name hint, empty when unset).
    pub logo: String,
}

/// Reads `/etc/os-release` then `/usr/lib/os-release`, returning the first one
/// that could be read. `None` when neither exists.
pub fn release() -> Option<Release> {
    PATHS.into_iter().find_map(read)
}

fn read(path: &str) -> Option<Release> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(parse(&text)),
        Err(e) => {
            debug!("{path} を読めません: {e}");
            None
        }
    }
}

/// Parses the `KEY=value` body of an `os-release` file.
///
/// Unknown keys are ignored: the specification keeps growing and a widget must
/// not care. `PRETTY_NAME` falls back to `NAME`, `version` prefers `VERSION_ID`
/// over `BUILD_ID`.
fn parse(text: &str) -> Release {
    let mut release = Release::default();
    let mut version_id = String::new();
    let mut build_id = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "ID" => release.id = value,
            "NAME" => release.name = value,
            "PRETTY_NAME" => release.pretty_name = value,
            "VERSION_ID" => version_id = value,
            "BUILD_ID" => build_id = value,
            "HOME_URL" => release.home_url = value,
            "LOGO" => release.logo = value,
            _ => {}
        }
    }

    if release.pretty_name.is_empty() {
        release.pretty_name.clone_from(&release.name);
    }
    release.version = if version_id.is_empty() {
        build_id
    } else {
        version_id
    };
    release
}

/// Strips one layer of quotes, then resolves the shell-style escapes the
/// specification allows: `\\`, `\"`, `\$`, `` \` `` and, inside single quotes,
/// `\'`. Anything else after a backslash is left as written, because the file
/// may be hand-edited and we would rather show the raw text than drop it.
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    let quote = match (chars.next(), chars.next_back()) {
        (Some(quote @ ('"' | '\'')), Some(last)) if quote == last => quote,
        _ => return trimmed.to_owned(),
    };
    let inner = &trimmed[1..trimmed.len() - 1];
    if !inner.contains('\\') {
        return inner.to_owned();
    }

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some(escaped) if is_escape(quote, escaped) => out.push(escaped),
            // An unknown or dangling escape: keep the backslash.
            Some(escaped) => {
                out.push('\\');
                out.push(escaped);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Whether `escaped` is an escape sequence for values quoted with `quote`.
fn is_escape(quote: char, escaped: char) -> bool {
    match escaped {
        '\\' | '"' | '$' | '`' => true,
        '\'' => quote == '\'',
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Arch Linux file, shortened to the keys we read.
    const ARCH: &str = r#"# /etc/os-release
NAME="Arch Linux"
PRETTY_NAME="Arch Linux"
ID=arch
ID_LIKE=arch
BUILD_ID=rolling
ANSI_COLOR="38;2;23;147;209"
HOME_URL="https://archlinux.org/"
DOCUMENTATION_URL="https://wiki.archlinux.org/"
LOGO=archlinux-logo
"#;

    #[test]
    fn double_quoted_values_lose_their_quotes() {
        let release = parse(ARCH);
        assert_eq!(release.id, "arch");
        assert_eq!(release.name, "Arch Linux");
        assert_eq!(release.pretty_name, "Arch Linux");
        assert_eq!(release.home_url, "https://archlinux.org/");
        assert_eq!(release.logo, "archlinux-logo");
    }

    #[test]
    fn single_quoted_values_lose_their_quotes() {
        let release = parse("ID='arch'\nPRETTY_NAME='Arch Linux'\n");
        assert_eq!(release.id, "arch");
        assert_eq!(release.pretty_name, "Arch Linux");
    }

    #[test]
    fn unquoted_values_are_kept_as_is() {
        let release = parse("ID=debian\nVERSION_ID=12\n");
        assert_eq!(release.id, "debian");
        assert_eq!(release.version, "12");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let release = parse("# ID=wrong\n\n   \n#LOGO=wrong\nID=arch\n");
        assert_eq!(release.id, "arch");
        assert!(release.logo.is_empty());
    }

    #[test]
    fn unterminated_quotes_are_left_alone() {
        let release = parse("NAME=\"Arch Linux\n");
        assert_eq!(release.name, "\"Arch Linux");
    }

    #[test]
    fn pretty_name_falls_back_to_name() {
        let release = parse("NAME=\"Arch Linux\"\nID=arch\n");
        assert_eq!(release.pretty_name, "Arch Linux");
    }

    #[test]
    fn version_falls_back_to_build_id() {
        let release = parse("ID=arch\nBUILD_ID=rolling\n");
        assert_eq!(release.version, "rolling");

        let preferred = parse("ID=arch\nVERSION_ID=42\nBUILD_ID=rolling\n");
        assert_eq!(preferred.version, "42");
    }

    #[test]
    fn escapes_are_decoded() {
        let release = parse(r#"NAME="My \"Arch\" \$HOME \`x\` \\""#);
        assert_eq!(release.name, "My \"Arch\" $HOME `x` \\");

        let single = parse(r"NAME='It\'s fine'");
        assert_eq!(single.name, "It's fine");
    }

    #[test]
    fn unknown_keys_do_not_break_parsing() {
        let release = parse("VARIANT_ID=desktop\nID=arch\nGARBAGE\n");
        assert_eq!(release.id, "arch");
    }

    #[test]
    fn the_machine_reports_a_release() {
        let Some(release) = release() else {
            // No os-release at all is valid (a chroot, a container).
            return;
        };
        // The fallback keeps these in step.
        assert_eq!(release.pretty_name.is_empty(), release.name.is_empty());
    }
}
