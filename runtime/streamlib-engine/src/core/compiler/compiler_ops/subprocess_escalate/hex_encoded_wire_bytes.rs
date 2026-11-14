// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Hex-encoded byte payloads on the escalate wire, decoded.

/// Decode lowercase hex into bytes, returning a clean error message on
/// any malformed character or odd-length input. Empty string decodes to
/// an empty Vec — the caller validates push-constant size separately
/// against the kernel's declaration.
pub(super) fn decode_hex(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err(format!(
            "expected even-length hex string, got {} characters",
            s.len()
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let nibble = |b: u8| -> std::result::Result<u8, String> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(format!(
                "non-hex character {:?} at byte position",
                b as char
            )),
        }
    };
    for pair in bytes.as_chunks::<2>().0 {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hex_round_trips_lowercase_and_mixed_case() {
        assert_eq!(decode_hex("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_hex("00").unwrap(), vec![0u8]);
        assert_eq!(decode_hex("ff").unwrap(), vec![0xff]);
        assert_eq!(
            decode_hex("DeAdBeEf").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            decode_hex("0123456789abcdef").unwrap(),
            vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
    }

    #[test]
    fn decode_hex_rejects_odd_length() {
        let err = decode_hex("abc").expect_err("expected odd-length error");
        assert!(err.contains("even-length"), "got: {err}");
    }

    #[test]
    fn decode_hex_rejects_non_hex_character() {
        let err = decode_hex("abxy").expect_err("expected non-hex error");
        assert!(err.contains("non-hex"), "got: {err}");
    }
}
