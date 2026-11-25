// SPDX-License-Identifier: GPL-3.0-or-later

//! The locale identity of the runtime: one locale, English (United States),
//! which is Xenora's choice. Everything here is documented public data: the
//! LCID and LANGID values of en-US from Microsoft's locale pages, and the
//! strings `GetLocaleInfo` answers for it.
//!
//! A single locale is honest rather than wrong: a program asking for
//! another fails cleanly instead of being handed American formatting under
//! a French name.

/// The LCID and LANGID of en-US, as Microsoft's locale documentation lists.
pub(super) const EN_US: u32 = 0x0409;

/// The locale's name in the BCP 47 form the Ex functions take.
pub(super) const EN_US_NAME: &[u16] = &[
    b'e' as u16,
    b'n' as u16,
    b'-' as u16,
    b'U' as u16,
    b'S' as u16,
    0,
];

/// One entry of the table: the type's value in winnls.h (public in the
/// mingw-w64 headers) and the string en-US answers, as Microsoft's
/// `GetLocaleInfo` examples and locale documentation give it.
const TABLE: &[(u32, &str)] = &[
    (0x0002, "English (United States)"), // LOCALE_SLANGUAGE
    (0x1001, "English"),                 // LOCALE_SENGLANGUAGE
    (0x0004, "English"),                 // LOCALE_SNATIVELANGUAGENAME
    (0x0003, "ENU"),                     // LOCALE_SABBREVLANGNAME
    (0x0006, "United States"),           // LOCALE_SCOUNTRY
    (0x1002, "United States"),           // LOCALE_SENGCOUNTRY
    (0x0008, "United States"),           // LOCALE_SNATIVECOUNTRYNAME
    (0x000C, ","),                       // LOCALE_SLIST
    (0x000E, "."),                       // LOCALE_SDECIMAL
    (0x000F, ","),                       // LOCALE_STHOUSAND
    (0x0014, "$"),                       // LOCALE_SCURRENCY
    (0x0015, "USD"),                     // LOCALE_SINTLSYMBOL
    (0x001D, "/"),                       // LOCALE_SDATE
    (0x001E, ":"),                       // LOCALE_STIME
    (0x001F, "M/d/yyyy"),                // LOCALE_SSHORTDATE
    (0x0020, "dddd, MMMM d, yyyy"),      // LOCALE_SLONGDATE
    (0x1003, "h:mm:ss tt"),              // LOCALE_STIMEFORMAT
    (0x005C, "en-US"),                   // LOCALE_SNAME
];

/// The mask of the modifier bits a caller may add: no user override (there
/// are no user overrides here, so it changes nothing) and the return-a-
/// number form, which none of the strings in the table takes.
const LOCALE_NOUSEROVERRIDE: u32 = 0x8000_0000;
const LOCALE_RETURN_NUMBER: u32 = 0x2000_0000;

/// The string a type answers, with the modifier bits resolved: user
/// overrides are absent, so that bit is simply dropped, and the number form
/// is not served for any entry.
pub(super) fn lookup(kind: u32) -> Option<&'static str> {
    if kind & LOCALE_RETURN_NUMBER != 0 {
        return None;
    }
    let kind = kind & !LOCALE_NOUSEROVERRIDE;
    TABLE.iter().find(|(k, _)| *k == kind).map(|(_, v)| *v)
}
