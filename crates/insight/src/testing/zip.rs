pub(crate) struct Entry<'a> {
    pub name: &'a str,
    pub data: Vec<u8>,
    pub deflated: bool,
    pub crc: u32,
    pub declared_size: u32,
}

impl<'a> Entry<'a> {
    pub fn stored(name: &'a str, data: &[u8]) -> Self {
        Self {
            name,
            data: data.to_vec(),
            deflated: false,
            crc: crc32(data.iter().copied()),
            declared_size: data.len() as u32,
        }
    }

    pub fn zeros_bomb(name: &'a str, repeats: u32, honest_size: bool) -> Self {
        let size = 1 + 258 * repeats;
        Self {
            name,
            data: deflate_zeros(repeats),
            deflated: true,
            crc: crc32(std::iter::repeat_n(0, size as usize)),
            declared_size: if honest_size { size } else { 1 },
        }
    }
}

pub(crate) fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let offset = out.len() as u32;
        let method: u16 = if entry.deflated { 8 } else { 0 };
        let name = entry.name.as_bytes();
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        for field in [20u16, 0, method, 0, 0x21] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        for field in [entry.crc, entry.data.len() as u32, entry.declared_size] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&entry.data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        for field in [20u16, 20, 0, method, 0, 0x21] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [entry.crc, entry.data.len() as u32, entry.declared_size] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        for field in [name.len() as u16, 0, 0, 0, 0] {
            central.extend_from_slice(&field.to_le_bytes());
        }
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    let count = entries.len() as u16;
    for field in [0u16, 0, count, count] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

struct Bits {
    out: Vec<u8>,
    current: u8,
    used: u8,
}

impl Bits {
    fn push(&mut self, bit: u8) {
        self.current |= bit << self.used;
        self.used += 1;
        if self.used == 8 {
            self.out.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    fn value(&mut self, value: u32, width: u8) {
        for i in 0..width {
            self.push(((value >> i) & 1) as u8);
        }
    }

    fn code(&mut self, code: u32, width: u8) {
        for i in (0..width).rev() {
            self.push(((code >> i) & 1) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push(self.current);
        }
        self.out
    }
}

/// One fixed-Huffman deflate block: a literal zero, then `repeats` copies of 258 bytes at
/// distance 1, so the output is `1 + 258 * repeats` zero bytes.
fn deflate_zeros(repeats: u32) -> Vec<u8> {
    let mut bits = Bits {
        out: Vec::new(),
        current: 0,
        used: 0,
    };
    bits.value(1, 1);
    bits.value(1, 2);
    bits.code(0b0011_0000, 8);
    for _ in 0..repeats {
        bits.code(0b1100_0101, 8);
        bits.code(0, 5);
    }
    bits.code(0, 7);
    bits.finish()
}

fn crc32(bytes: impl Iterator<Item = u8>) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    !bytes.fold(!0u32, |crc, byte| {
        table[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789".iter().copied()), 0xCBF4_3926);
    }

    #[test]
    fn archive_has_both_directories() {
        let bytes = archive(&[Entry::stored("pass.json", b"{}")]);
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(&bytes[bytes.len() - 22..bytes.len() - 18], b"PK\x05\x06");
    }
}
