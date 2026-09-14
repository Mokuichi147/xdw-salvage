//! zlib compression for PDF/PNG streams and the CRC-32 used by PNG chunks.

/// Compress `data` into a zlib stream.
///
/// The exact compressed bytes are not part of the output contract. Consumers
/// only require a valid zlib stream, so use the maintained pure-Rust encoder
/// instead of maintaining a DEFLATE implementation here.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// Adler-32 used by a zlib stream.
pub fn adler32(data: &[u8]) -> u32 {
    miniz_oxide::mz_adler32_oxide(miniz_oxide::MZ_ADLER32_INIT, data)
}

/// CRC-32 as PNG uses it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(data: &[u8]) {
        let z = zlib(data);
        assert_eq!(&z[..2], &[0x78, 0x9C]);
        let back = miniz_oxide::inflate::decompress_to_vec_zlib(&z)
            .expect("miniz_oxide should decode its zlib output");
        assert_eq!(back, data);
        let tail = &z[z.len() - 4..];
        assert_eq!(u32::from_be_bytes(tail.try_into().unwrap()), adler32(data));
    }

    #[test]
    fn empty_and_tiny_inputs_round_trip() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"ab");
        round_trip(b"abc");
    }

    #[test]
    fn repeated_bytes_become_matches_and_come_back() {
        let data = vec![0xFFu8; 100_000];
        let z = zlib(&data);
        assert!(
            z.len() < 1000,
            "a flat page should shrink hard: {}",
            z.len()
        );
        round_trip(&data);
    }

    #[test]
    fn mixed_content_round_trips() {
        let mut data = Vec::new();
        for i in 0..20_000u32 {
            data.push((i.wrapping_mul(2654435761) >> 13) as u8);
            if i % 7 == 0 {
                data.extend_from_slice(b"the same phrase again and again");
            }
        }
        round_trip(&data);
    }

    #[test]
    fn matches_across_the_maximum_length_and_far_distances_round_trip() {
        let mut data: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        data.extend_from_slice(&data.clone()[..5000]);
        round_trip(&data);
    }

    #[test]
    fn the_checksums_match_the_standard_test_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
