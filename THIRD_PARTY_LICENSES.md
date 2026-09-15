# 第三者コンポーネントのライセンス

## encoding_rs 0.8.41

CP932（Windows-31J）のデコードに [`encoding_rs`](https://github.com/hsivonen/encoding_rs) を使用しています。

- コードの著作権: Mozilla Foundation
- 生成された符号化データの著作権: WHATWG（Apple、Google、Mozilla、Microsoft）
- コードのライセンス: Apache-2.0 または MIT
- 生成された符号化データのライセンス: BSD-3-Clause

`encoding_rs` の配布物には、上記のライセンス本文と著作権表示が含まれています。バイナリを再配布する場合も、以下の表示とライセンス条件を保持してください。

### BSD-3-Clause（生成された符号化データ）

Copyright © WHATWG (Apple, Google, Mozilla, Microsoft).

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

コード部分のライセンス本文は、[`encoding_rs` の配布物](https://crates.io/crates/encoding_rs/0.8.41)に含まれる `LICENSE-APACHE` および `LICENSE-MIT` を参照してください。

## miniz_oxide 0.8.9

zlib / DEFLATE 圧縮に [`miniz_oxide`](https://github.com/Frommi/miniz_oxide) を使用しています。

- ライセンス: MIT、Zlib、または Apache-2.0
- `miniz_oxide` のライセンス本文と著作権表示は、[配布物](https://crates.io/crates/miniz_oxide/0.8.9)に含まれる `LICENSE`、`LICENSE-ZLIB.md`、`LICENSE-APACHE.md` を参照してください。

### adler2 2.0.1

`adler2` は `miniz_oxide` が使用する間接依存です。

- ライセンス: 0BSD、MIT、または Apache-2.0
- [配布物](https://crates.io/crates/adler2/2.0.1)のライセンス本文と著作権表示を保持してください。

## subsetter 0.2.6

`--font` で指定されたTrueTypeフォントを、PDFで使用するグリフだけに
サブセット化するために使用しています。

- ライセンス: MIT または Apache-2.0
- [配布物](https://crates.io/crates/subsetter/0.2.6)のライセンス本文と著作権表示を保持してください。

### rustc-hash 2.1.3

`rustc-hash` は `subsetter` が使用する間接依存です。

- ライセンス: MIT または Apache-2.0
- [配布物](https://crates.io/crates/rustc-hash/2.1.3)のライセンス本文と著作権表示を保持してください。

## フォントファイル

本プロジェクトはフォントファイルを同梱・再配布しません。`--font` で指定した
フォントは出力PDFに埋め込まれるため、そのフォントのライセンスが埋め込みと
PDFの再配布を許可しているかを利用者が確認してください。サブセット化後も、
フォントに含まれる著作権情報は保持されます。
