//! URL text: a URL's scheme, the userinfo (`user:token@`) that can carry a
//! credential, found and redacted, and a remote URL's host and path as git
//! connects for it (`remote_parts`).
//!
//! For the registry parser, the runner's anonymous read, classification,
//! and the scan alike. Pure string work; no URL here is fetched or
//! resolved.

use std::borrow::Cow;

/// A URL's scheme, lowercased: what precedes `://`, when that's a plain
/// scheme name. `None` for anything else, scp-like SSH syntax included.
pub(crate) fn url_scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once("://")?;
    let plain = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    plain.then(|| scheme.to_ascii_lowercase())
}

/// `url`'s origin, `<scheme>://<authority>` (the authority up to the first
/// `/`, port included); `None` without a `<scheme>://`.
pub(crate) fn url_origin(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    url_scheme(url)?;
    let end = scheme.len() + 3 + rest.find('/').unwrap_or(rest.len());
    Some(&url[..end])
}

/// Whether `host` is a plain DNS name: letters, digits, `.`, and `-`,
/// starting and ending with a letter or digit — no userinfo, port,
/// IP-literal brackets, or escapes.
pub(crate) fn is_plain_host(host: &str) -> bool {
    host.starts_with(|c: char| c.is_ascii_alphanumeric())
        && host.ends_with(|c: char| c.is_ascii_alphanumeric())
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
}

/// A remote URL's parts as git reaches them: the host it connects to, the
/// port if one is named, and the path on that host (trailing `/`s and one
/// `.git` dropped), read structurally (`remote_parts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteParts<'a> {
    pub host: &'a str,
    pub port: Option<&'a str>,
    pub path: &'a str,
    /// Reached over SSH: an `ssh://` URL (or git's `git+ssh://` and
    /// `ssh+git://` spellings), or scp-like syntax.
    pub ssh: bool,
}

/// `url`'s host, port, and path, where git would connect for it; `None`
/// for anything that isn't plainly a remote repo URL.
///
/// Two forms, as git tells them apart. A URL, `<scheme>://<authority>/<path>`
/// with the scheme one of `ssh`, `git+ssh`, `ssh+git`, `git`, `https`, or
/// `http` (lowercase, as git's transport lookup reads it): the authority
/// runs to the first `/`, and only it may carry `user@` and a `:port`. The
/// scp-like form, `[user@]host:path` with no `://` and a `:` before any
/// `/`: the host runs to the first `:`, and only it may carry `user@`.
///
/// Fails closed: a host that isn't a plain DNS name (`is_plain_host`: an
/// `@` left in it, an IP literal), a user that's empty, could read as an
/// option, or holds anything but ASCII letters, digits, and `._+-`, an
/// empty path, any `%` anywhere — git decodes a URL's
/// escapes before splitting it, so an escaped `/` or `@` could move the
/// host — and any `[` or `]` anywhere — git unwraps a bracketed run at the
/// start of the host text, `user@` included, and scans on for `@[` into
/// the path, so a bracket can move the host too — all read as `None`.
/// Nothing is decoded or resolved here.
pub(crate) fn remote_parts(url: &str) -> Option<RemoteParts<'_>> {
    if url.contains(['%', '[', ']']) {
        return None;
    }
    let (ssh, host, port, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let ssh = match scheme {
            "ssh" | "git+ssh" | "ssh+git" => true,
            "git" | "https" | "http" => false,
            _ => return None,
        };
        let (authority, path) = rest.split_once('/')?;
        let host_port = without_user(authority)?;
        let (host, port) = match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        };
        (ssh, host, port, path)
    } else {
        // scp-like: the host runs to the first `:`, and has no port
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (true, without_user(authority)?, None, path)
    };
    if !is_plain_host(host) {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!path.is_empty()).then_some(RemoteParts {
        host,
        port,
        path,
        ssh,
    })
}

/// `authority` less its `user@`, if the user is plain: ASCII letters,
/// digits, and `._+-`, not empty and not an option (`-…`). `None`
/// otherwise. Split at the first `@`, so any `@` after it stays in the
/// host, which then isn't plain.
fn without_user(authority: &str) -> Option<&str> {
    let Some((user, host)) = authority.split_once('@') else {
        return Some(authority);
    };
    let plain = !user.is_empty()
        && !user.starts_with('-')
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
    plain.then_some(host)
}

/// Whether `url`'s authority carries userinfo (`user:token@host`), whatever
/// the scheme.
pub(crate) fn has_userinfo(url: &str) -> bool {
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
pub(crate) fn without_userinfo(url: &str) -> Cow<'_, str> {
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
pub(crate) fn redact_userinfo_in(text: &str) -> String {
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
pub(crate) fn escape_ere(s: &str) -> String {
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
    fn remote_parts_are_where_git_connects() {
        let parts = |host, port, path, ssh| {
            Some(RemoteParts {
                host,
                port,
                path,
                ssh,
            })
        };
        for (url, want) in [
            (
                "git@github.com:me/app",
                parts("github.com", None, "me/app", true),
            ),
            (
                "git@GitHub.COM:Me/App.git/",
                parts("GitHub.COM", None, "Me/App", true),
            ),
            ("gh:me/app", parts("gh", None, "me/app", true)),
            (
                "ssh://git@github.com/me/app.git",
                parts("github.com", None, "me/app", true),
            ),
            (
                "git+ssh://github.com/me/app",
                parts("github.com", None, "me/app", true),
            ),
            (
                "ssh+git://git@github.com/me/app",
                parts("github.com", None, "me/app", true),
            ),
            (
                "ssh://git@github.com:22/me/app",
                parts("github.com", Some("22"), "me/app", true),
            ),
            (
                "https://ghp_TOKEN@github.com/me/app/",
                parts("github.com", None, "me/app", false),
            ),
            (
                "git://github.com/me/app.git",
                parts("github.com", None, "me/app", false),
            ),
            // an `@` past the authority is the path's: git connects to the
            // host before it
            (
                "ssh://evil.com/x@github.com/me/app",
                parts("evil.com", None, "x@github.com/me/app", true),
            ),
            (
                "ssh+git://evil.com/@github.com/me/app",
                parts("evil.com", None, "@github.com/me/app", true),
            ),
            (
                "evil.com:x@github.com/me/app",
                parts("evil.com", None, "x@github.com/me/app", true),
            ),
            (
                "git@evil.com:git@github.com:me/app",
                parts("evil.com", None, "git@github.com:me/app", true),
            ),
            // an IPv4 address is a plain host, compared as the text it is
            (
                "https://127.0.0.1:1/me/app",
                parts("127.0.0.1", Some("1"), "me/app", false),
            ),
        ] {
            assert_eq!(remote_parts(url), want, "{url}");
        }
        for url in [
            // git decodes escapes first: this one connects to `evil.com`
            "ssh://evil.com%2F@github.com/me/app",
            "ssh://git%40evil.com@github.com/me/app",
            "git@github.com:me%2Fapp",
            // an `@` left in the host
            "ssh://a@b@github.com/me/app",
            "a@b@github.com:me/app",
            // IP literals and brackets
            "git@[::1]:me/app",
            "ssh://git@[::1]/me/app",
            "[git@github.com]:me/app",
            // a bracketed run in the user: git connects to `evil.com`
            "ssh://[evil.com]x@github.com/me/app",
            "ssh://[evil.com]x@github.com:2222/me/app",
            "[evil.com]x@github.com:me/app",
            // `@[` in the path: git's scan runs into it, connecting to `x`
            "git@github.com:me/app@[x]:y",
            // a user outside the plain characters
            "https://me:ghp_TOKEN@github.com/me/app",
            "ssh://g\u{e9}@github.com/me/app",
            "g\\t@github.com:me/app",
            // a user that's empty or reads as an option
            "@github.com:me/app",
            "-oProxyCommand=x@github.com:me/app",
            "ssh://-oProxyCommand=x@github.com/me/app",
            // schemes git spells otherwise, or that reach no remote host
            "SSH://github.com/me/app",
            "file:///srv/me/app.git",
            "ftp://github.com/me/app",
            // no path, or no host
            "https://github.com",
            "https://github.com/",
            "git@github.com:",
            "ssh:///me/app",
            // local paths
            "/home/me/app",
            "../app",
            "./me:app",
            "",
            // an odd host
            "git@github.com.:me/app",
            "git@git hub.com:me/app",
        ] {
            assert_eq!(remote_parts(url), None, "{url}");
        }
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
