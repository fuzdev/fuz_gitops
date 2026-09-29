//! URL text: a URL's scheme, and the userinfo (`user:token@`) that can
//! carry a credential, found and redacted.
//!
//! For the registry parser, the runner's anonymous read, classification,
//! and the scan alike. Pure string work; no URL here is fetched or
//! resolved.

use std::borrow::Cow;

/// A URL's scheme, lowercased: what precedes `://`, when that's a plain
/// scheme name. `None` for anything else, scp-like SSH syntax included.
pub fn url_scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once("://")?;
    let plain = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    plain.then(|| scheme.to_ascii_lowercase())
}

/// `url`'s origin, `<scheme>://<authority>` (the authority up to the first
/// `/`, port included); `None` without a `<scheme>://`.
pub fn url_origin(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    url_scheme(url)?;
    let end = scheme.len() + 3 + rest.find('/').unwrap_or(rest.len());
    Some(&url[..end])
}

/// Whether `url`'s authority carries userinfo (`user:token@host`), whatever
/// the scheme.
pub fn has_userinfo(url: &str) -> bool {
    url.split_once("://").is_some_and(|(_, rest)| {
        rest.split('/')
            .next()
            .is_some_and(|authority| authority.contains('@'))
    })
}

/// Whether the userinfo `user` of a `scheme` URL may carry a credential:
/// any userinfo but an SSH login name (`ssh://git@host` names an account;
/// SSH takes no password in the URL, so a `:` there is redacted too).
fn may_carry_credential(scheme: &str, user: &str) -> bool {
    let ssh = matches!(
        scheme.to_ascii_lowercase().as_str(),
        "ssh" | "git+ssh" | "ssh+git"
    );
    !ssh || user.contains(':')
}

/// `authority` with a credential-bearing userinfo replaced by `***`.
fn redact_authority<'a>(scheme: &str, authority: &'a str) -> Cow<'a, str> {
    match authority.rfind('@') {
        Some(at) if may_carry_credential(scheme, &authority[..at]) => {
            format!("***{}", &authority[at..]).into()
        }
        _ => authority.into(),
    }
}

/// `url` with a credential-bearing userinfo in its authority replaced by
/// `***`, so an error or report never repeats a credential.
///
/// An SSH login name stays; a URL with no `://` (scp-like `git@host:path`,
/// a path) is returned as is.
pub fn without_userinfo(url: &str) -> Cow<'_, str> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.into();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match redact_authority(scheme, &rest[..end]) {
        Cow::Borrowed(_) => url.into(),
        Cow::Owned(authority) => format!("{scheme}://{authority}{}", &rest[end..]).into(),
    }
}

/// `text` with the userinfo of every `<scheme>://` URL in it redacted as
/// `without_userinfo` does.
///
/// For messages that quote source text, like a TOML parse error's snippet
/// of the offending line. A URL's authority ends at `/`, a quote, or
/// whitespace; its scheme is the run of scheme characters before `://`.
pub fn redact_userinfo_in(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let scheme_start = head[..i]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || "+-.".contains(c)))
            .map_or(0, |j| j + 1);
        let scheme = &head[scheme_start..i];
        let end = tail
            .find(|c: char| c == '/' || c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(tail.len());
        out.push_str(&redact_authority(scheme, &tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// `s` as a POSIX extended regex matching it literally, for git's
/// value-pattern argument (`git config --unset-all <key> <pattern>`).
pub fn escape_ere(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_is_found_and_redacted() {
        assert!(has_userinfo("https://user:tok@github.com/a/b"));
        assert!(has_userinfo("http://tok@127.0.0.1:1/a/b"));
        assert!(has_userinfo("ssh://git@github.com/a/b"));
        assert!(!has_userinfo("https://github.com/a/b@c"));
        assert!(!has_userinfo("https://github.com/a/b"));
        for (url, want) in [
            (
                "https://user:tok@github.com/a/b",
                "https://***@github.com/a/b",
            ),
            (
                "https://ghp_TOKEN@github.com/old/a",
                "https://***@github.com/old/a",
            ),
            ("https://github.com/a/b", "https://github.com/a/b"),
            // an SSH login name isn't a secret; a password there is
            ("ssh://git@github.com/a/b", "ssh://git@github.com/a/b"),
            ("ssh://git:pw@github.com/a/b", "ssh://***@github.com/a/b"),
            ("git@github.com:a/b", "git@github.com:a/b"),
            ("/srv/repos/a.git", "/srv/repos/a.git"),
        ] {
            assert_eq!(without_userinfo(url), want, "{url}");
        }
        assert_eq!(
            redact_userinfo_in(
                "4 | url = \"https://user:sekrit@github.com/me/app\"\nand ftp://a@b c://d/e@f \
                 ssh://git@h/x"
            ),
            "4 | url = \"https://***@github.com/me/app\"\nand ftp://***@b c://d/e@f \
             ssh://git@h/x"
        );
    }

    #[test]
    fn schemes_and_patterns() {
        assert_eq!(url_scheme("HTTPS://x/y").as_deref(), Some("https"));
        assert_eq!(url_scheme("git@github.com:a/b"), None);
        assert_eq!(url_scheme("1http://x"), None);
        assert_eq!(escape_ere(r"a.b+c\d(e)"), r"a\.b\+c\\d\(e\)");
        assert_eq!(
            url_origin("https://github.com/a/b"),
            Some("https://github.com")
        );
        assert_eq!(
            url_origin("http://127.0.0.1:1/a/b"),
            Some("http://127.0.0.1:1")
        );
        assert_eq!(url_origin("file:///srv/a"), Some("file://"));
        assert_eq!(url_origin("https://h"), Some("https://h"));
        assert_eq!(url_origin("git@github.com:a/b"), None);
    }
}
