use anyhow::Result;

pub struct Compressor;

impl Compressor {
    pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
        let compressed = zstd::encode_all(data, 3)?;
        Ok(compressed)
    }

    pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
        let decompressed = zstd::decode_all(data)?;
        Ok(decompressed)
    }

    pub fn compress_stream<R: std::io::Read, W: std::io::Write>(reader: R, writer: W, level: i32) -> Result<()> {
        let mut encoder = zstd::Encoder::new(writer, level)?;
        std::io::copy(&mut std::io::BufReader::new(reader), &mut encoder)?;
        encoder.finish()?;
        Ok(())
    }

    pub fn decompress_stream<R: std::io::Read, W: std::io::Write>(reader: R, writer: W) -> Result<()> {
        let mut decoder = zstd::Decoder::new(reader)?;
        std::io::copy(&mut decoder, &mut std::io::BufWriter::new(writer))?;
        Ok(())
    }

    pub fn estimate_compressed_size(original_size: u64) -> u64 {
        (original_size as f64 * 0.6) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress() {
        let original = b"Hello, World! This is a test string for compression. Hello, World! This is a test string for compression. Hello, World! This is a test string for compression.";
        let compressed = Compressor::compress(original).unwrap();
        let decompressed = Compressor::decompress(&compressed).unwrap();

        assert_eq!(original.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_compress_large_data() {
        let original = vec![b'A'; 10000];
        let compressed = Compressor::compress(&original).unwrap();
        let decompressed = Compressor::decompress(&compressed).unwrap();

        assert_eq!(original, decompressed);
        assert!(compressed.len() < original.len());
    }

    #[test]
    fn test_compress_empty_data() {
        let original = b"";
        let compressed = Compressor::compress(original).unwrap();
        let decompressed = Compressor::decompress(&compressed).unwrap();

        assert_eq!(original.as_slice(), decompressed.as_slice());
    }

    #[test]
    fn test_compress_random_data() {
        let original: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let compressed = Compressor::compress(&original).unwrap();
        let decompressed = Compressor::decompress(&compressed).unwrap();

        assert_eq!(original, decompressed);
    }

    #[test]
    fn test_estimate_compressed_size() {
        assert_eq!(Compressor::estimate_compressed_size(1000), 600);
        assert_eq!(Compressor::estimate_compressed_size(10000), 6000);
        assert_eq!(Compressor::estimate_compressed_size(0), 0);
    }
}
