/// Module that implements the Pike stream cipher used in TERA.
use byteorder::{ByteOrder, LittleEndian};

use crate::crypt::sha1::Sha1;

// Provides a struct for the stream cipher used by TERA.
pub struct StreamCipher {
    generators: [KeyGenerator; 3],
    change_data: u32,
    change_len: usize,
}

impl StreamCipher {
    /// Construct a `StreamCipher` object. Key must be 128 byte in size.
    pub fn new(key: &[u8]) -> StreamCipher {
        let mut sc = StreamCipher {
            generators: [
                KeyGenerator::new(55, 31),
                KeyGenerator::new(57, 50),
                KeyGenerator::new(58, 39),
            ],
            change_data: 0,
            change_len: 0,
        };

        // Expand the given key using the botched SHA1 implementation.
        let mut expanded_key = [0; 680];
        expanded_key[0] = 128;
        for i in 1..680 {
            expanded_key[i] = key[i % 128];
        }
        for i in (0..680).step_by(20) {
            let mut sha = Sha1::new();
            sha.update(&expanded_key);
            let hash = sha.hash().unwrap();
            for j in (0..20).step_by(4) {
                LittleEndian::write_u32(&mut expanded_key[i + j..], hash[j / 4]);
            }
        }

        // Set the initial state of the KeyGenerators.
        for i in 0..55 {
            sc.generators[0].buffer[i] = LittleEndian::read_u32(&expanded_key[i * 4..]);
        }
        for i in 0..57 {
            sc.generators[1].buffer[i] = LittleEndian::read_u32(&expanded_key[(i * 4 + 220)..]);
        }
        for i in 0..58 {
            sc.generators[2].buffer[i] = LittleEndian::read_u32(&expanded_key[(i * 4 + 448)..]);
        }
        sc
    }

    /// Applies the StreamCipher on the data. The data needs to be at least 4 bytes in size.
    #[inline]
    pub fn apply_keystream(&mut self, data: &mut [u8]) {
        let size = data.len();
        let pre = if size < self.change_len {
            size
        } else {
            self.change_len
        };

        if pre != 0 {
            for (i, el) in data.iter_mut().take(pre).enumerate() {
                let shift = 8 * (4 - self.change_len + i);
                *el ^= (self.change_data >> shift) as u8;
            }
            self.change_len -= pre;
        }

        for i in (pre..size - 3).step_by(4) {
            self.clock_keys();
            for k in self.generators.iter() {
                data[i] ^= k.sum as u8;
                data[i + 1] ^= (k.sum >> 8) as u8;
                data[i + 2] ^= (k.sum >> 16) as u8;
                data[i + 3] ^= (k.sum >> 24) as u8;
            }
        }

        let remain = (size - pre) & 3;
        if remain != 0 {
            self.clock_keys();
            self.change_data = 0;
            for k in self.generators.iter() {
                self.change_data ^= k.sum;
            }

            for i in 0..remain {
                data[size - remain + i] ^= (self.change_data >> (i * 8)) as u8;
            }

            self.change_len = 4 - remain;
        }
    }

    #[inline]
    fn clock_keys(&mut self) {
        let key_clock = self.generators[0].carry & self.generators[1].carry
            | self.generators[2].carry & (self.generators[0].carry | self.generators[1].carry);
        for k in self.generators.iter_mut() {
            if key_clock == k.carry {
                let pos1 = k.buffer[k.pos1 as usize];
                let pos2 = k.buffer[k.pos2 as usize];

                // Calculate next sum + test carry used for clocking
                let (sum, carry) = pos1.overflowing_add(pos2);
                k.carry = carry;
                k.sum = sum;

                // Advance both positions
                k.pos1 = (k.pos1 + 1) % k.size as u32;
                k.pos2 = (k.pos2 + 1) % k.size as u32;
            }
        }
    }
}

/// Fibonacci key generator.
struct KeyGenerator {
    pub size: usize,
    pub pos1: u32,
    pub pos2: u32,
    pub carry: bool,
    pub buffer: Vec<u32>,
    pub sum: u32,
}

impl KeyGenerator {
    /// Construct a `KeyGenerator` object
    pub fn new(size: usize, coefficient: u32) -> KeyGenerator {
        KeyGenerator {
            size,
            pos1: 0,
            pos2: coefficient,
            carry: false,
            buffer: vec![0; size],
            sum: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use hex::encode;

    use super::StreamCipher;

    fn setup_cipher() -> StreamCipher {
        let key: [u8; 128] = [0x12; 128];
        StreamCipher::new(&key)
    }

    #[test]
    fn test_cipher_repeat() {
        let mut cipher = setup_cipher();

        let mut data: [u8; 32] = [0xce; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "1b429bb891e2a631190550a609d2a815ddb58d0866ce2d7bb3894246c4c26d0d",
        );

        data = [0x00; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "1eb1321c0cb111044a7264336dc9521c8c18bbe6b5af4ee227cce206990d60ef",
        );

        data = [0xff; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "fe07bb243a80a783caf91a7907978534efff975bd080ff39b1f3df04bd24f02d",
        );
    }

    #[test]
    fn test_cipher_repeat_alternative_order() {
        let mut cipher = setup_cipher();

        let mut data: [u8; 32] = [0x00; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "d58c55765f2c68ffd7cb9e68c71c66db137b43c6a800e3b57d478c880a0ca3c3",
        );

        data = [0xce; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "d07ffcd2c27fdfca84bcaafda3079cd242d675287b61802ce9022cc857c3ae21",
        );

        data = [0xff; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "fe07bb243a80a783caf91a7907978534efff975bd080ff39b1f3df04bd24f02d",
        );
    }

    #[test]
    fn test_cipher_00_data() {
        let mut cipher = setup_cipher();

        let mut data: [u8; 32] = [0x00; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "d58c55765f2c68ffd7cb9e68c71c66db137b43c6a800e3b57d478c880a0ca3c3",
        );
    }

    #[test]
    fn test_cipher_ff_data() {
        let mut cipher = setup_cipher();

        let mut data: [u8; 32] = [0xff; 32];
        cipher.apply_keystream(&mut data);
        assert_eq!(
            encode(&data),
            "2a73aa89a0d397002834619738e39924ec84bc3957ff1c4a82b87377f5f35c3c",
        );
    }

    #[test]
    fn test_cipher_4_byte() {
        let mut cipher = setup_cipher();

        let mut data: [u8; 4] = [0x11; 4];
        cipher.apply_keystream(&mut data);
        assert_eq!(encode(&data), "c49d4467");
    }
}
