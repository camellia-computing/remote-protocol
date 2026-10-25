use std::{
    cell::RefCell,
    io::{self, Read},
};
use zstd::bulk::Compressor;

pub(crate) const MAX_DECOMPRESSED_SIZE: usize = 256 * 1024 * 1024;

// The library supports regular compression levels from 1 up to ZSTD_maxCLevel(),
// which is currently 22. Levels >= 20
// Default level is ZSTD_CLEVEL_DEFAULT==3.
// value 0 means default, which is controlled by ZSTD_CLEVEL_DEFAULT
thread_local! {
    static COMPRESSOR: RefCell<io::Result<Compressor<'static>>> = RefCell::new(Compressor::new(crate::config::COMPRESS_LEVEL));
}

pub fn compress(data: &[u8]) -> io::Result<Vec<u8>> {
    COMPRESSOR.with(|c| {
        let mut compressor = c.try_borrow_mut().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "file compressor is already borrowed",
            )
        })?;
        match &mut *compressor {
            Ok(compressor) => compressor.compress(data),
            Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
        }
    })
}

pub fn decompress(data: &[u8]) -> io::Result<Vec<u8>> {
    decompress_with_limit(data, MAX_DECOMPRESSED_SIZE)
}

pub(crate) fn decompress_with_limit(data: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    let decoder = zstd::Decoder::new(data)?;
    let mut output = Vec::new();
    decoder
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "decompressed data exceeds size limit",
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_data_larger_than_limit() {
        let compressed = zstd::encode_all(&vec![0u8; 1025][..], 0).unwrap();
        assert!(decompress_with_limit(&compressed, 1024).is_err());
    }

    #[test]
    fn accepts_data_at_limit() {
        let input = vec![0u8; 1024];
        let compressed = zstd::encode_all(&input[..], 0).unwrap();
        assert_eq!(
            decompress_with_limit(&compressed, input.len()).unwrap(),
            input
        );
    }

    #[test]
    fn compressor_contention_must_not_be_reported_as_empty_success() {
        COMPRESSOR.with(|compressor| {
            let _borrow = compressor.borrow_mut();
            assert!(
                compress(b"payload must not disappear").is_err(),
                "compressor contention must not become a successful empty payload"
            );
        });
    }
}
