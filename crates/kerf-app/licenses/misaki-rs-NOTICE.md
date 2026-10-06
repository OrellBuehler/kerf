# misaki-rs (voiceover)

Kerf's voiceover feature compiles in [misaki-rs](https://github.com/MicheleYin/misaki-rs),
a Rust port of the [misaki](https://github.com/hexgrad/misaki) grapheme-to-phoneme
engine, together with the English lexicons and part-of-speech tagger weights it
embeds. It is built **without** its optional espeak-ng fallback, so no espeak-ng
code is part of this distribution. See `Cargo.lock` for the exact version.

The text-to-speech model itself ([Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M),
Apache License 2.0) and the [ONNX Runtime](https://github.com/microsoft/onnxruntime)
library that runs it (MIT License) are downloaded separately at runtime, the
first time a voiceover is generated, and are not part of this distribution.

misaki-rs is licensed under the **MIT License**, reproduced below.

---

MIT License

Copyright (c) 2026 Michele Yin

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
