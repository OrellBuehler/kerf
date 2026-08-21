# whisper.cpp

Release builds of Kerf are compiled with the `whisper` cargo feature, which
**statically links** [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
(and ggml) for on-device speech-to-text, via the
[`whisper-rs`](https://codeberg.org/tazz4843/whisper-rs) bindings. The version
built into this distribution is the one vendored by `whisper-rs-sys` — see
`Cargo.lock` for the exact crate revision.

whisper.cpp is licensed under the **MIT License**, reproduced below. The speech
models it runs are downloaded separately at runtime and are not part of this
distribution.

---

MIT License

Copyright (c) 2023-2024 The ggml authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
