// SPDX-License-Identifier: GPL-3.0-or-later

use std::{net::Ipv6Addr, ops::Range};

pub(super) const ERROR_WINHTTP_INVALID_URL: u32 = 12005;
pub(super) const ERROR_WINHTTP_UNRECOGNIZED_SCHEME: u32 = 12006;

pub(super) struct ParsedUrl {
    pub(super) scheme: Range<usize>,
    pub(super) scheme_id: u32,
    pub(super) host: Range<usize>,
    pub(super) port: u16,
    pub(super) username: Option<Range<usize>>,
    pub(super) password: Option<Range<usize>>,
    pub(super) path: Range<usize>,
    pub(super) extra: Range<usize>,
}

pub(super) fn parse(url: &[u16], reject_credentials: bool) -> Result<ParsedUrl, u32> {
    if url
        .iter()
        .any(|&unit| matches!(unit, 0..=0x20 | 0x5c | 0x7f))
    {
        return Err(ERROR_WINHTTP_INVALID_URL);
    }

    let scheme_end = find(url, b':').ok_or(ERROR_WINHTTP_INVALID_URL)?;
    let (scheme_id, default_port) = parse_scheme(&url[..scheme_end])?;
    let authority = url[scheme_end + 1..]
        .strip_prefix(&[u16::from(b'/'), u16::from(b'/')])
        .ok_or(ERROR_WINHTTP_INVALID_URL)?;
    let authority_start = scheme_end + 3;
    let authority_len = authority
        .iter()
        .position(|&unit| matches!(u8::try_from(unit), Ok(b'/' | b'?' | b'#')))
        .unwrap_or(authority.len());
    let tail_start = authority_start + authority_len;
    let authority = &authority[..authority_len];

    let (username, password, host_start) = if let Some(at) = find(authority, b'@') {
        let userinfo = &authority[..at];
        if reject_credentials || !valid_authority_text(userinfo, true) {
            return Err(ERROR_WINHTTP_INVALID_URL);
        }
        let userinfo_end = authority_start + at;
        let separator = find(userinfo, b':').map(|offset| authority_start + offset);
        (
            Some(authority_start..separator.unwrap_or(userinfo_end)),
            separator.map(|colon| colon + 1..userinfo_end),
            userinfo_end + 1,
        )
    } else {
        (None, None, authority_start)
    };

    let host_and_port = &url[host_start..tail_start];
    let host_len = host_length(host_and_port)?;
    let port = parse_port(&host_and_port[host_len..], default_port)?;

    let extra_start = url[tail_start..]
        .iter()
        .position(|&unit| matches!(u8::try_from(unit), Ok(b'?' | b'#')))
        .map_or(url.len(), |offset| tail_start + offset);

    Ok(ParsedUrl {
        scheme: 0..scheme_end,
        scheme_id,
        host: host_start..host_start + host_len,
        port,
        username,
        password,
        path: tail_start..extra_start,
        extra: extra_start..url.len(),
    })
}

fn find(units: &[u16], ascii: u8) -> Option<usize> {
    units.iter().position(|&unit| unit == u16::from(ascii))
}

fn parse_scheme(scheme: &[u16]) -> Result<(u32, u16), u32> {
    let mut ascii = scheme.iter().copied().map(u8::try_from);
    if !ascii
        .next()
        .is_some_and(|byte| byte.is_ok_and(|byte| byte.is_ascii_alphabetic()))
        || !ascii.all(|byte| {
            byte.is_ok_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
            })
        })
    {
        return Err(ERROR_WINHTTP_INVALID_URL);
    }

    let is_scheme = |expected: &[u8]| {
        scheme.len() == expected.len()
            && scheme.iter().zip(expected).all(|(&unit, &byte)| {
                u8::try_from(unit).is_ok_and(|ascii| ascii.eq_ignore_ascii_case(&byte))
            })
    };
    if is_scheme(b"http") {
        Ok((1, 80))
    } else if is_scheme(b"https") {
        Ok((2, 443))
    } else {
        Err(ERROR_WINHTTP_UNRECOGNIZED_SCHEME)
    }
}

// RFC 3986 sections 3.2.1 and 3.2.2 delimit userinfo and registered
// names. Preserve non-ASCII code units for WinHTTP's UTF-16 interface.
fn valid_authority_text(mut units: &[u16], allow_colon: bool) -> bool {
    while let Some((&unit, rest)) = units.split_first() {
        units = rest;
        if unit > 0x7f {
            continue;
        }
        let byte = unit as u8;
        if byte == b'%' {
            let Some((escape, remaining)) = units.split_at_checked(2) else {
                return false;
            };
            if !escape
                .iter()
                .all(|&unit| u8::try_from(unit).is_ok_and(|digit| digit.is_ascii_hexdigit()))
            {
                return false;
            }
            units = remaining;
        } else if !(byte.is_ascii_alphanumeric()
            || b"-._~!$&'()*+,;=".contains(&byte)
            || allow_colon && byte == b':')
        {
            return false;
        }
    }
    true
}

fn host_length(host_and_port: &[u16]) -> Result<usize, u32> {
    if host_and_port.first() == Some(&u16::from(b'[')) {
        let close = find(host_and_port, b']').ok_or(ERROR_WINHTTP_INVALID_URL)?;
        let literal =
            String::from_utf16(&host_and_port[1..close]).map_err(|_| ERROR_WINHTTP_INVALID_URL)?;
        literal
            .parse::<Ipv6Addr>()
            .map_err(|_| ERROR_WINHTTP_INVALID_URL)?;
        return Ok(close + 1);
    }

    let host_len = find(host_and_port, b':').unwrap_or(host_and_port.len());
    if host_len == 0 || !valid_authority_text(&host_and_port[..host_len], false) {
        return Err(ERROR_WINHTTP_INVALID_URL);
    }
    Ok(host_len)
}

fn parse_port(suffix: &[u16], default_port: u16) -> Result<u16, u32> {
    if suffix.is_empty() {
        return Ok(default_port);
    }
    let digits = suffix
        .strip_prefix(&[u16::from(b':')])
        .filter(|digits| !digits.is_empty())
        .ok_or(ERROR_WINHTTP_INVALID_URL)?;
    digits.iter().try_fold(0u16, |port, &unit| {
        let digit = match u8::try_from(unit) {
            Ok(byte @ b'0'..=b'9') => u16::from(byte - b'0'),
            _ => return Err(ERROR_WINHTTP_INVALID_URL),
        };
        port.checked_mul(10)
            .and_then(|port| port.checked_add(digit))
            .ok_or(ERROR_WINHTTP_INVALID_URL)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(url: &str) -> Vec<u16> {
        url.encode_utf16().collect()
    }

    #[test]
    fn preserves_original_utf16_parts() {
        let url = wide("HtTpS://ü:p%40:ss@[2001:db8::1]:065535/🚀%20?q=@/#片");
        let parsed = parse(&url, false).unwrap();
        assert_eq!(&url[parsed.scheme], wide("HtTpS"));
        assert_eq!(parsed.scheme_id, 2);
        assert_eq!(&url[parsed.host], wide("[2001:db8::1]"));
        assert_eq!(parsed.port, 65535);
        assert_eq!(&url[parsed.username.unwrap()], wide("ü"));
        assert_eq!(&url[parsed.password.unwrap()], wide("p%40:ss"));
        assert_eq!(&url[parsed.path], wide("/🚀%20"));
        assert_eq!(&url[parsed.extra], wide("?q=@/#片"));
    }

    #[test]
    fn locates_empty_paths_and_keeps_authority_defaults() {
        for suffix in ["", "?x=/a", "#frag?x"] {
            let url = wide(&format!("HTTP://example.test{suffix}"));
            let parsed = parse(&url, true).unwrap();
            assert_eq!(parsed.scheme_id, 1);
            assert_eq!(parsed.port, 80);
            assert_eq!(parsed.path, 19..19);
            assert_eq!(&url[parsed.extra], wide(suffix));
            assert!(parsed.username.is_none());
            assert!(parsed.password.is_none());
        }
        let url = wide("https://例.test");
        let parsed = parse(&url, true).unwrap();
        assert_eq!(parsed.port, 443);
        assert_eq!(&url[parsed.host], wide("例.test"));
    }

    #[test]
    fn splits_optional_credentials_and_rejects_them_on_request() {
        for (authority, username, password) in [
            ("u@host", "u", None),
            (":p@host", "", Some("p")),
            ("u:@host", "u", Some("")),
        ] {
            let url = wide(&format!("https://{authority}"));
            let parsed = parse(&url, false).unwrap();
            assert_eq!(&url[parsed.username.unwrap()], wide(username));
            assert_eq!(
                parsed.password.map(|range| &url[range]),
                password.map(wide).as_deref()
            );
            assert_eq!(parse(&url, true).err(), Some(ERROR_WINHTTP_INVALID_URL));
        }
    }

    #[test]
    fn distinguishes_unknown_schemes_from_malformed_urls() {
        assert_eq!(
            parse(&wide("ftp://host/"), false).err(),
            Some(ERROR_WINHTTP_UNRECOGNIZED_SCHEME)
        );
        for url in [
            "host/path",
            "1http://host/",
            "http:/host",
            "http:///path",
            "http://u@u@host/",
            "http://u@/",
            "http://host:/",
            "http://host:65536/",
            "http://host:+80/",
            "http://host:８０/",
            "http://2001:db8::1/",
            "http://[::1]host/",
            "http://[::g]/",
            "http://[::1/",
            "http://host]/",
            "http://%GG/",
            "http://some host/",
            "http://host/line\nfeed",
            "http://host/\0",
            "http://host\\path",
        ] {
            assert_eq!(
                parse(&wide(url), false).err(),
                Some(ERROR_WINHTTP_INVALID_URL),
                "{url:?}"
            );
        }
    }
}
