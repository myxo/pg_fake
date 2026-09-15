pub(super) fn calculate_text_hash(value: &str) -> i32 {
    calculate_hash_state(value.as_bytes(), None).1 as i32
}

pub(super) fn calculate_extended_text_hash(value: &str, seed: i64) -> i64 {
    let (high, low) = calculate_hash_state(value.as_bytes(), Some(seed as u64));
    ((u64::from(high) << 32) | u64::from(low)) as i64
}

fn calculate_hash_state(bytes: &[u8], seed: Option<u64>) -> (u32, u32) {
    let length = u32::try_from(bytes.len()).expect("text length must fit in uint32");
    let initial = 0x9e37_79b9_u32.wrapping_add(length).wrapping_add(3_923_095);
    let (mut a, mut b, mut c) = (initial, initial, initial);
    if let Some(seed) = seed.filter(|seed| *seed != 0) {
        a = a.wrapping_add((seed >> 32) as u32);
        b = b.wrapping_add(seed as u32);
        mix_state(&mut a, &mut b, &mut c);
    }

    let mut chunks = bytes.chunks_exact(12);
    for chunk in &mut chunks {
        a = a.wrapping_add(u32::from_le_bytes(chunk[0..4].try_into().unwrap()));
        b = b.wrapping_add(u32::from_le_bytes(chunk[4..8].try_into().unwrap()));
        c = c.wrapping_add(u32::from_le_bytes(chunk[8..12].try_into().unwrap()));
        mix_state(&mut a, &mut b, &mut c);
    }
    let remainder = chunks.remainder();
    for (index, byte) in remainder.iter().copied().enumerate() {
        match index {
            0..=3 => a = a.wrapping_add(u32::from(byte) << (index * 8)),
            4..=7 => b = b.wrapping_add(u32::from(byte) << ((index - 4) * 8)),
            8..=10 => c = c.wrapping_add(u32::from(byte) << ((index - 7) * 8)),
            _ => unreachable!("chunks_exact remainder must contain fewer than 12 bytes"),
        }
    }
    finalize_state(&mut a, &mut b, &mut c);
    (b, c)
}

fn mix_state(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(4);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(6);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(8);
    *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c);
    *a ^= c.rotate_left(16);
    *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a);
    *b ^= a.rotate_left(19);
    *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b);
    *c ^= b.rotate_left(4);
    *b = b.wrapping_add(*a);
}

fn finalize_state(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(14));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(11));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(25));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(16));
    *a ^= *c;
    *a = a.wrapping_sub(c.rotate_left(4));
    *b ^= *a;
    *b = b.wrapping_sub(a.rotate_left(14));
    *c ^= *b;
    *c = c.wrapping_sub(b.rotate_left(24));
}
