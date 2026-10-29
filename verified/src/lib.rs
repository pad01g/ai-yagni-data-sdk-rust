//! SDK のうち、Verus で証明する部分。
//!
//! SQL に差し込む表名・列名・データベース名は、引用符で囲む前にここで判定する。
//! 判定が仕様（`is_identifier_spec`）とちょうど一致することを証明している。
// Verus の検証器が仕様を知っている形（比較の連なり、len）で書くので、clippy の言い換えの提案は当てない。
#![allow(clippy::manual_range_contains, clippy::len_zero)]

use vstd::prelude::*;

verus! {

/// MySQL の識別子の長さの上限。
pub const MAX_IDENTIFIER: usize = 64;

pub open spec fn starts_identifier(b: u8) -> bool {
    (65 <= b <= 90) || (97 <= b <= 122) || b == 95
}

pub open spec fn continues_identifier(b: u8) -> bool {
    starts_identifier(b) || (48 <= b <= 57)
}

/// 英字か _ で始まり、英数字と _ だけが続く、64 バイト以下の列。
pub open spec fn is_identifier_spec(s: Seq<u8>) -> bool {
    0 < s.len() <= MAX_IDENTIFIER
        && starts_identifier(s[0])
        && forall|i: int| 0 <= i < s.len() ==> continues_identifier(#[trigger] s[i])
}

fn starts(b: u8) -> (r: bool)
    ensures r == starts_identifier(b),
{
    (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b == 95
}

fn continues(b: u8) -> (r: bool)
    ensures r == continues_identifier(b),
{
    starts(b) || (b >= 48 && b <= 57)
}

/// s が識別子として安全に引用できるか。
pub fn is_identifier(s: &[u8]) -> (r: bool)
    ensures r == is_identifier_spec(s@),
{
    if s.len() == 0 || s.len() > MAX_IDENTIFIER || !starts(s[0]) {
        return false;
    }
    let mut i: usize = 0;
    while i < s.len()
        invariant
            0 <= i <= s.len(),
            forall|j: int| 0 <= j < i ==> continues_identifier(#[trigger] s@[j]),
        decreases s.len() - i,
    {
        if !continues(s[i]) {
            return false;
        }
        i += 1;
    }
    true
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        assert!(is_identifier(b"orders"));
        assert!(is_identifier(b"_x9"));
        assert!(!is_identifier(b""));
        assert!(!is_identifier(b"9x"));
        assert!(!is_identifier(b"a-b"));
        assert!(!is_identifier(b"a`b"));
        assert!(!is_identifier(&[b'a'; 65]));
    }
}
