//! sysfs prints kernel unsigned-long words, not fixed-width concatenated hex digits.
use std::collections::BTreeSet;

pub(crate) fn parse(text: &str, word_bits: u32) -> Result<BTreeSet<u16>, &'static str> {
    if !matches!(word_bits, 32 | 64) {
        return Err("unknown kernel word width");
    }
    if text.len() > 4096 {
        return Err("bitmap exceeds the sysfs metadata bound");
    }
    let words: Vec<_> = text.split_whitespace().collect();
    if words.is_empty() {
        return Err("empty or truncated bitmap");
    }
    let mut bits = BTreeSet::new();
    for (index, word) in words.iter().rev().enumerate() {
        if word.len() > (word_bits / 4) as usize
            || !word.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid bitmap word");
        }
        let value = u64::from_str_radix(word, 16).map_err(|_| "overflowing bitmap word")?;
        for bit in 0..word_bits {
            if value & (1u64 << bit) != 0 {
                let code = index
                    .checked_mul(word_bits as usize)
                    .and_then(|base| base.checked_add(bit as usize))
                    .and_then(|code| u16::try_from(code).ok())
                    .ok_or("bitmap code overflows u16")?;
                bits.insert(code);
            }
        }
    }
    Ok(bits)
}
