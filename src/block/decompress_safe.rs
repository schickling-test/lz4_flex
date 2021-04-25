//! The decompression algorithm.

use core::convert::TryInto;

use crate::block::DecompressError;
use crate::block::Sink;
use crate::block::MINMATCH;
use alloc::vec::Vec;

/// Read an integer LSIC (linear small integer code) encoded.
///
/// In LZ4, we encode small integers in a way that we can have an arbitrary number of bytes. In
/// particular, we add the bytes repeatedly until we hit a non-0xFF byte. When we do, we add
/// this byte to our sum and terminate the loop.
///
/// # Example
///
/// ```notest
///     255, 255, 255, 4, 2, 3, 4, 6, 7
/// ```
///
/// is encoded to _255 + 255 + 255 + 4 = 769_. The bytes after the first 4 is ignored, because
/// 4 is the first non-0xFF byte.
#[inline]
fn read_integer(input: &[u8], input_pos: &mut usize) -> Result<u32, DecompressError> {
    // We start at zero and count upwards.
    let mut n: u32 = 0;
    // If this byte takes value 255 (the maximum value it can take), another byte is read
    // and added to the sum. This repeats until a byte lower than 255 is read.
    loop {
        // We add the next byte until we get a byte which we add to the counting variable.
        let extra: u8 = *input
            .get(*input_pos)
            .ok_or(DecompressError::ExpectedAnotherByte)?;
        *input_pos += 1;
        n += extra as u32;

        // We continue if we got 255, break otherwise.
        if extra != 0xFF {
            break;
        }
    }

    // 255, 255, 255, 8
    // 111, 111, 111, 101

    Ok(n)
}

/// Read a little-endian 16-bit integer from the input stream.
#[inline]
fn read_u16(input: &[u8], input_pos: &mut usize) -> Result<u16, DecompressError> {
    let dst = input
        .get(*input_pos..*input_pos + 2)
        .ok_or(DecompressError::ExpectedAnotherByte)?;
    *input_pos += 2;
    Ok(u16::from_le_bytes(dst.try_into().unwrap()))
}

const FIT_TOKEN_MASK_LITERAL: u8 = 0b00001111;
const FIT_TOKEN_MASK_MATCH: u8 = 0b11110000;

#[test]
fn check_token() {
    assert_eq!(does_token_fit(15), false);
    assert_eq!(does_token_fit(14), true);
    assert_eq!(does_token_fit(114), true);
    assert_eq!(does_token_fit(0b11110000), false);
    assert_eq!(does_token_fit(0b10110000), true);
}

/// The algorithm can copy over the original size, because of blocked copies,
/// so the capacity of the sink may need to be slightly larger.
#[inline]
fn decompress_sink_size(uncompressed_size: usize) -> usize {
    uncompressed_size
}

/// The token consists of two parts, the literal length (upper 4 bits) and match_length (lower 4 bits)
/// if the literal length and match_length are both below 15, we don't need to read additional data, so the token does fit the metadata.
#[inline]
fn does_token_fit(token: u8) -> bool {
    !((token & FIT_TOKEN_MASK_LITERAL) == FIT_TOKEN_MASK_LITERAL
        || (token & FIT_TOKEN_MASK_MATCH) == FIT_TOKEN_MASK_MATCH)
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
pub fn decompress_into(input: &[u8], output: &mut Sink) -> Result<usize, DecompressError> {
    decompress_internal::<false>(input, output, b"")
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
pub fn decompress_into_with_dict(
    input: &[u8],
    output: &mut Sink,
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    decompress_internal::<true>(input, output, ext_dict)
}

/// Decompress all bytes of `input` into `output`.
///
/// Returns the number of bytes written (decompressed) into `output`.
#[inline]
fn decompress_internal<const USE_DICT: bool>(
    input: &[u8],
    output: &mut Sink,
    ext_dict: &[u8],
) -> Result<usize, DecompressError> {
    // Decode into our vector.
    let mut input_pos = 0;
    let initial_output_pos = output.pos();

    let safe_input_pos = input
        .len()
        .saturating_sub(16 /* literal copy */ +  2 /* u16 match offset */);
    let safe_output_pos = output
        .capacity()
        .saturating_sub(16 /* literal copy */ + 18 /* match copy */);

    // Exhaust the decoder by reading and decompressing all blocks until the remaining buffer is empty.
    loop {
        // Read the token. The token is the first byte in a block. It is divided into two 4-bit
        // subtokens, the higher and the lower.
        // This token contains to 4-bit "fields", a higher and a lower, representing the literals'
        // length and the back reference's length, respectively. LSIC is used if either are their
        // maximal values.
        let token = *input
            .get(input_pos)
            .ok_or(DecompressError::ExpectedAnotherByte)?;
        input_pos += 1;

        // Checking for hot-loop.
        // In most cases the metadata does fit in a single 1byte token (statistically) and we are in a safe-distance to the end.
        // This enables some optimized handling.
        //
        // Ideally we want to check for safe output pos like: output.pos() <= safe_output_pos; But that doesn't work when the
        // safe_output_pos is 0 due to saturated_sub. so we use `<` instead of `<=`, which will cover that case.
        if does_token_fit(token) && input_pos <= safe_input_pos && output.pos() < safe_output_pos {
            let literal_length = (token >> 4) as usize;

            if input_pos + literal_length > input.len() {
                return Err(DecompressError::LiteralOutOfBounds);
            }

            // Copy the literal
            // The literal is at max 14 bytes, and the is_safe_distance check assures
            // that we are far away enough from the end so we can safely copy 16 bytes
            {
                let pos = output.pos();
                output.output[pos..pos + 16].copy_from_slice(&input[input_pos..input_pos + 16]);
            }
            output.pos += literal_length;
            input_pos += literal_length;

            let offset = read_u16(input, &mut input_pos)? as usize;

            let mut match_length = MINMATCH + (token & 0xF) as usize;

            if USE_DICT && offset > output.pos() {
                let copied = copy_from_dict(output, ext_dict, offset, match_length)?;
                if copied == match_length {
                    continue;
                }
                // match crosses ext_dict and output, offset is still correct as output pos increased
                match_length -= copied;
            }

            // In this branch we know that match_length is at most 18 (14 + MINMATCH).
            // But the blocks can overlap, so make sure they are at least 20 bytes apart
            // to enable an optimized copy of 18 bytes.
            let (start, did_overflow) = output.pos().overflowing_sub(offset);
            if did_overflow {
                return Err(DecompressError::OffsetOutOfBounds);
            }
            if offset >= match_length {
                output.output.copy_within(start..start + 18, output.pos());
                output.pos += match_length;
            } else {
                for i in start..start + match_length {
                    let b = output.as_slice()[i];
                    output.push(b);
                }
            }

            continue;
        }

        // Now, we read the literals section.
        // Literal Section
        // If the initial value is 15, it is indicated that another byte will be read and added to it
        let mut literal_length = (token >> 4) as usize;
        if literal_length != 0 {
            if literal_length == 15 {
                // The literal_length length took the maximal value, indicating that there is more than 15
                // literal_length bytes. We read the extra integer.
                literal_length += read_integer(input, &mut input_pos)? as usize;
            }

            if input_pos + literal_length > input.len() {
                return Err(DecompressError::LiteralOutOfBounds);
            }
            #[cfg(feature = "checked-decode")]
            if output.pos + literal_length > output.capacity() {
                return Err(DecompressError::OutputTooSmall {
                    expected_size: output.pos + literal_length,
                    actual_size: output.capacity(),
                });
            }
            output.extend_from_slice(&input[input_pos..input_pos + literal_length]);
            input_pos += literal_length;
        }

        // If the input stream is emptied, we break out of the loop. This is only the case
        // in the end of the stream, since the block is intact otherwise.
        if input_pos >= input.len() {
            break;
        }

        let offset = read_u16(input, &mut input_pos)? as usize;
        // Obtain the initial match length. The match length is the length of the duplicate segment
        // which will later be copied from data previously decompressed into the output buffer. The
        // initial length is derived from the second part of the token (the lower nibble), we read
        // earlier. Since having a match length of less than 4 would mean negative compression
        // ratio, we start at 4 (MINMATCH).

        // The initial match length can maximally be 19. As with the literal length, this indicates
        // that there are more bytes to read.
        let mut match_length = MINMATCH + (token & 0xF) as usize;
        if match_length == MINMATCH + 15 {
            // The match length took the maximal value, indicating that there is more bytes. We
            // read the extra integer.
            match_length += read_integer(input, &mut input_pos)? as usize;
        }

        #[cfg(feature = "checked-decode")]
        if output.pos + match_length > output.capacity() {
            return Err(DecompressError::OutputTooSmall {
                expected_size: output.pos + match_length,
                actual_size: output.capacity(),
            });
        }
        if USE_DICT && offset > output.pos() {
            let copied = copy_from_dict(output, ext_dict, offset, match_length)?;
            if copied == match_length {
                continue;
            }
            // match crosses ext_dict and output, offset is still correct as output_len was increased
            match_length -= copied;
        }
        // We now copy from the already decompressed buffer. This allows us for storing duplicates
        // by simply referencing the other location.
        duplicate_slice(output, offset, match_length)?;
    }
    Ok(output.pos() - initial_output_pos)
}

#[inline]
fn copy_from_dict(
    output: &mut Sink,
    ext_dict: &[u8],
    offset: usize,
    match_length: usize,
) -> Result<usize, DecompressError> {
    // If we're here we know offset > output.pos
    debug_assert!(offset > output.pos);
    let (dict_offset, did_overflow) = ext_dict.len().overflowing_sub(offset - output.pos());
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    // Can't copy past ext_dict len, the match may cross dict and output
    let dict_match_length = match_length.min(ext_dict.len() - dict_offset);
    let ext_match = &ext_dict[dict_offset..dict_offset + dict_match_length];
    output.extend_from_slice(ext_match);
    Ok(dict_match_length)
}

/// Extends output by self-referential copies
#[inline(always)] // (always) necessary otherwise compiler fails to inline it
fn duplicate_slice(
    output: &mut Sink,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    // This function assumes output will fit match_length, it might panic otherwise.
    if match_length > offset {
        duplicate_overlapping_slice(output, offset, match_length)?;
    } else {
        let (start, did_overflow_1) = output.pos().overflowing_sub(offset);
        if did_overflow_1 {
            return Err(DecompressError::OffsetOutOfBounds);
        }
        let output_end = output.pos() + match_length;
        if output_end + (16 - 1) <= output.capacity() {
            for i in (start..start + match_length).step_by(16) {
                output.output.copy_within(i..i + 16, output.pos());
                output.pos += 16;
            }
        } else {
            output
                .output
                .copy_within(start..start + match_length, output.pos());
        }
        output.set_pos(output_end);
    }
    Ok(())
}

/// self-referential copy for the case data start (end of output - offset) + match_length overlaps into output
#[inline]
fn duplicate_overlapping_slice(
    sink: &mut Sink,
    offset: usize,
    match_length: usize,
) -> Result<(), DecompressError> {
    // This function assumes output will fit match_length, it might panic otherwise.
    let (start, did_overflow) = sink.pos().overflowing_sub(offset);
    if did_overflow {
        return Err(DecompressError::OffsetOutOfBounds);
    }
    if offset == 1 {
        let val = sink.as_slice()[start];
        let dest = sink.pos();
        sink.output[dest..dest + match_length].fill(val);
        sink.pos += match_length;
    } else {
        for i in start..start + match_length {
            let b = sink.as_slice()[i];
            sink.push(b);
        }
    }
    Ok(())
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in litte endian.
/// Can be used in conjunction with `compress_prepend_size`
#[inline]
pub fn decompress_size_prepended(input: &[u8]) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress(input, uncompressed_size)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress(input: &[u8], uncompressed_size: usize) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec: Vec<u8> = Vec::with_capacity(uncompressed_size);
    vec.resize(decompress_sink_size(uncompressed_size), 0);
    let mut sink: Sink = (&mut vec).into();
    let decomp_len = decompress_into(input, &mut sink)?;
    if decomp_len != uncompressed_size {
        return Err(DecompressError::UncompressedSizeDiffers {
            expected: uncompressed_size,
            actual: decomp_len,
        });
    }
    Ok(vec)
}

/// Decompress all bytes of `input` into a new vec. The first 4 bytes are the uncompressed size in little endian.
/// Can be used in conjunction with `compress_prepend_size_with_dict`
#[inline]
pub fn decompress_size_prepended_with_dict(
    input: &[u8],
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    let (uncompressed_size, input) = super::uncompressed_size(input)?;
    decompress_with_dict(input, uncompressed_size, ext_dict)
}

/// Decompress all bytes of `input` into a new vec.
#[inline]
pub fn decompress_with_dict(
    input: &[u8],
    uncompressed_size: usize,
    ext_dict: &[u8],
) -> Result<Vec<u8>, DecompressError> {
    // Allocate a vector to contain the decompressed stream.
    let mut vec: Vec<u8> = Vec::with_capacity(uncompressed_size);
    vec.resize(decompress_sink_size(uncompressed_size), 0);
    let mut sink: Sink = (&mut vec).into();
    let decomp_len = decompress_into_with_dict(input, &mut sink, ext_dict)?;
    if decomp_len != uncompressed_size {
        return Err(DecompressError::UncompressedSizeDiffers {
            expected: uncompressed_size,
            actual: decomp_len,
        });
    }
    Ok(vec)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn all_literal() {
        assert_eq!(decompress(&[0x30, b'a', b'4', b'9'], 3).unwrap(), b"a49");
    }

    // this error test is only valid in safe-decode.
    #[cfg(feature = "safe-decode")]
    #[test]
    fn offset_oob() {
        decompress(&[0x10, b'a', 2, 0], 4).unwrap_err();
        decompress(&[0x40, b'a', 1, 0], 4).unwrap_err();
    }
}
