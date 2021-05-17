0.8.0 (2021-17-05)
==================
Support for the lz4 frame format, with a massive amount of improvements. (big thanks to @arthurprs)
~40% faster safe decompression, ~10% faster safe compression.
* PR-13 https://github.com/PSeitz/lz4_flex/pull/13

Added the possibility to compress/decompress into a u8 slice with
for `compress_into` and `decompress_into`. Previously it only accepted
a `Vec` with caused some double allocations for some scenarios. It's 
around 10% faster for compressible data with safe-decoding, which makes sense, since
push has additional overhead to check the capacity which is not required.

* BUG-11 (https://github.com/PSeitz/lz4_flex/issues/11)
	Allow to compress/decompress into a u8 slice

