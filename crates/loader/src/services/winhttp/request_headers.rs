// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_WINHTTP_HEADER_NOT_FOUND,
    ERROR_WINHTTP_HEADER_SIZE_OVERFLOW, MAX_HEADER_LINE_BYTES,
};

// WinHTTP request-header modifiers from the public Windows SDK winhttp.h.
const ADD_IF_NEW: u32 = 0x1000_0000;
const ADD: u32 = 0x2000_0000;
const COALESCE_WITH_COMMA: u32 = 0x4000_0000;
const COALESCE_WITH_SEMICOLON: u32 = 0x0100_0000;
const REPLACE: u32 = 0x8000_0000;
const REPLACE_INDEX_MASK: u32 = 0xff;
const KNOWN_FLAGS: u32 = ADD_IF_NEW | ADD | COALESCE_WITH_COMMA | COALESCE_WITH_SEMICOLON | REPLACE;

const ERROR_WINHTTP_HEADER_ALREADY_EXISTS: u32 = 12155;
const MAX_REQUEST_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_HEADER_COUNT: usize = 256;

pub(super) fn update(
    headers: &mut Vec<(String, String)>,
    additions: Vec<(String, String)>,
    modifiers: u32,
) -> Result<(), u32> {
    // The SDK specifies the low eight bits for a replacement occurrence.
    let occurrence = (modifiers & REPLACE_INDEX_MASK) as usize;
    let flags = modifiers & !REPLACE_INDEX_MASK;
    if flags & !KNOWN_FLAGS != 0 || (occurrence != 0 && flags & REPLACE == 0) {
        return Err(ERROR_NOT_SUPPORTED);
    }
    let operation = flags & !ADD;
    let separator = match operation {
        0 | ADD_IF_NEW | REPLACE => None,
        COALESCE_WITH_COMMA => Some(", "),
        COALESCE_WITH_SEMICOLON => Some("; "),
        _ => return Err(ERROR_INVALID_PARAMETER),
    };
    if operation == REPLACE && additions.len() != 1 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    if additions.is_empty() {
        return Ok(());
    }

    let mut updated = headers.clone();
    for (name, value) in additions {
        let matching_index = updated
            .iter()
            .enumerate()
            .filter(|(_, (existing, _))| existing.eq_ignore_ascii_case(&name))
            .nth(occurrence)
            .map(|(index, _)| index);
        match (operation, matching_index, separator) {
            (ADD_IF_NEW, Some(_), _) => return Err(ERROR_WINHTTP_HEADER_ALREADY_EXISTS),
            (REPLACE, Some(index), _) if value.is_empty() => {
                updated.remove(index);
            }
            (REPLACE, Some(index), _) => {
                updated[index].1 = value;
            }
            (REPLACE, None, _) if occurrence != 0 || flags & ADD == 0 || value.is_empty() => {
                return Err(ERROR_WINHTTP_HEADER_NOT_FOUND);
            }
            (_, Some(index), Some(separator)) => {
                updated[index].1.push_str(separator);
                updated[index].1.push_str(&value);
            }
            _ => updated.push((name, value)),
        }
    }

    if updated.len() > MAX_REQUEST_HEADER_COUNT {
        return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
    }
    let mut wire_bytes = 0;
    for (name, value) in &updated {
        let line_bytes = name.len() + 2 + value.len(); // Includes the colon and space.
        if line_bytes > MAX_HEADER_LINE_BYTES {
            return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
        }
        wire_bytes += line_bytes + 2; // Includes CRLF.
    }
    if wire_bytes > MAX_REQUEST_HEADER_BYTES {
        return Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW);
    }

    *headers = updated;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn appends_duplicates_and_coalesces_only_the_first_match() {
        let mut stored = headers(&[("X-List", "one")]);
        update(&mut stored, headers(&[("x-list", "two")]), 0).unwrap();
        update(&mut stored, headers(&[("X-LIST", "three")]), ADD).unwrap();
        update(
            &mut stored,
            headers(&[("x-LIST", "four"), ("Other", "one")]),
            COALESCE_WITH_COMMA,
        )
        .unwrap();
        update(
            &mut stored,
            headers(&[("X-list", "five")]),
            ADD | COALESCE_WITH_SEMICOLON,
        )
        .unwrap();
        assert_eq!(
            stored,
            headers(&[
                ("X-List", "one, four; five"),
                ("x-list", "two"),
                ("X-LIST", "three"),
                ("Other", "one")
            ])
        );
    }

    #[test]
    fn replaces_and_removes_the_first_match_or_adds_a_missing_header() {
        let mut stored = headers(&[("X-List", "first"), ("x-list", "second")]);
        update(&mut stored, headers(&[("X-LIST", "new")]), REPLACE).unwrap();
        assert_eq!(stored, headers(&[("X-List", "new"), ("x-list", "second")]));
        update(&mut stored, headers(&[("X-LIST", "")]), REPLACE | ADD).unwrap();
        assert_eq!(stored, headers(&[("x-list", "second")]));
        assert_eq!(
            update(&mut stored, headers(&[("Other", "new")]), REPLACE),
            Err(ERROR_WINHTTP_HEADER_NOT_FOUND)
        );
        assert_eq!(
            update(&mut stored, headers(&[("Other", "")]), REPLACE | ADD),
            Err(ERROR_WINHTTP_HEADER_NOT_FOUND)
        );
        update(&mut stored, headers(&[("Other", "added")]), REPLACE | ADD).unwrap();
        assert_eq!(stored, headers(&[("x-list", "second"), ("Other", "added")]));
    }

    #[test]
    fn indexed_replace_and_remove_preserve_other_occurrences_and_fields() {
        let mut stored = headers(&[
            ("X-List", "one"),
            ("Other", "keep"),
            ("x-list", "two"),
            ("X-LIST", "three"),
        ]);
        update(&mut stored, headers(&[("X-LIST", "changed")]), REPLACE | 1).unwrap();
        update(&mut stored, headers(&[("x-list", "")]), REPLACE | 2).unwrap();
        let expected = headers(&[("X-List", "one"), ("Other", "keep"), ("x-list", "changed")]);
        assert_eq!(stored, expected);
        assert_eq!(
            update(
                &mut stored,
                headers(&[("X-LIST", "new")]),
                REPLACE | ADD | 2
            ),
            Err(ERROR_WINHTTP_HEADER_NOT_FOUND)
        );
        assert_eq!(stored, expected);

        let mut full = headers(&[("X-List", "old"); MAX_REQUEST_HEADER_COUNT]);
        update(&mut full, headers(&[("X-List", "last")]), REPLACE | 255).unwrap();
        assert_eq!(full[254].1, "old");
        assert_eq!(full[255].1, "last");
    }

    #[test]
    fn add_if_new_rolls_back_a_batch_with_a_later_duplicate() {
        let original = headers(&[("Existing", "one")]);
        let mut stored = original.clone();
        assert_eq!(
            update(
                &mut stored,
                headers(&[("New", "one"), ("EXISTING", "two")]),
                ADD_IF_NEW
            ),
            Err(ERROR_WINHTTP_HEADER_ALREADY_EXISTS)
        );
        assert_eq!(stored, original);

        assert_eq!(
            update(
                &mut stored,
                headers(&[("New", "one"), ("nEW", "two")]),
                ADD_IF_NEW
            ),
            Err(ERROR_WINHTTP_HEADER_ALREADY_EXISTS)
        );
        assert_eq!(stored, original);
        update(&mut stored, headers(&[("New", "one")]), ADD_IF_NEW).unwrap();
        assert_eq!(stored, headers(&[("Existing", "one"), ("New", "one")]));
    }

    #[test]
    fn rejects_unsupported_or_conflicting_modifiers_and_multiple_replacements() {
        let original = headers(&[("X-List", "first"), ("x-list", "second")]);
        let mut stored = original.clone();
        let addition = headers(&[("X-List", "new")]);
        for (flags, error) in [
            (REPLACE | 0x100, ERROR_NOT_SUPPORTED),
            (COALESCE_WITH_COMMA | 1, ERROR_NOT_SUPPORTED),
            (0x0200_0000, ERROR_NOT_SUPPORTED),
            (ADD_IF_NEW | REPLACE, ERROR_INVALID_PARAMETER),
            (REPLACE | COALESCE_WITH_COMMA, ERROR_INVALID_PARAMETER),
            (
                COALESCE_WITH_COMMA | COALESCE_WITH_SEMICOLON,
                ERROR_INVALID_PARAMETER,
            ),
        ] {
            assert_eq!(update(&mut stored, addition.clone(), flags), Err(error));
            assert_eq!(stored, original);
        }
        assert_eq!(
            update(
                &mut stored,
                headers(&[("X-List", "one"), ("X-Other", "two")]),
                REPLACE,
            ),
            Err(ERROR_INVALID_PARAMETER)
        );
        assert_eq!(stored, original);
    }

    #[test]
    fn bounds_the_result_and_rolls_back_overflow() {
        let largest_value = "v".repeat(MAX_HEADER_LINE_BYTES - "X: ".len());
        let mut stored = Vec::new();
        update(&mut stored, headers(&[("X", largest_value.as_str())]), ADD).unwrap();
        let original = stored.clone();
        assert_eq!(
            update(
                &mut stored,
                headers(&[("New", "ok"), ("x", "overflow")]),
                COALESCE_WITH_COMMA,
            ),
            Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW)
        );
        assert_eq!(stored, original);

        let original = vec![original[0].clone(); 7];
        stored = original.clone();
        assert_eq!(
            update(&mut stored, headers(&[("X", largest_value.as_str())]), ADD),
            Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW)
        );
        assert_eq!(stored, original);

        let original = vec![("X".to_owned(), "v".to_owned()); MAX_REQUEST_HEADER_COUNT];
        stored = original.clone();
        assert_eq!(
            update(&mut stored, headers(&[("New", "overflow")]), ADD),
            Err(ERROR_WINHTTP_HEADER_SIZE_OVERFLOW)
        );
        assert_eq!(stored, original);
    }
}
