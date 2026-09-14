# xdw-salvage

DocuWorks 形式（`.xdw` / `.xbd`）のコンテナを解析し、ベンダー製品なしで回収できるデータを取り出す Rust クレート／CLI です。依存クレートはなく、`unsafe` も使用していません。

> **非公式ツールです。** FUJIFILM Business Innovation とは無関係で、同社の承認・支援・提携を受けていません。DocuWorks は同社の製品名です。

## できること

| 機能 | 内容 |
|---|---|
| ページ画像 | 回収可能な JPEG を再エンコードせず、バイト単位で取り出す |
| 同梱された元ファイル | PDF、docx / xlsx / pptx、旧 Office 形式などを切り出す |
| ページ本文・図版 | ベンダー独自符号化を展開し、中の EMF / WMF を再現する。文字、画像配置、埋め込み DIB の帯、罫線・塗り、輪郭パスによるクリップ・塗り・縁取り、太字・下線 |
| 回転・注釈 | 文書プロパティから表示用紙・回転角を読み、画像ページに重なる図形や注釈（テキスト・矩形）を描く |
| PDF / HTML | 回収結果を PDF または外部ファイルに依存しない HTML にまとめる |
| 移行監査 | 構造表示、期待ページ数の算出、変換後のページ数照合、資産分類を行う |

ページ表にはシートだけでなく、縮小画像やページ上の画像も含まれます。本ツールはこれらを区別し、縮小画像を実ページとして二重に数えないようにします。詳しい解析結果と実測値は [`docs/coding.md`](docs/coding.md) を参照してください。

## アーキテクチャ

依存方向を内側へ向け、文書モデル・ユースケース・具体実装・出力形式を分離しています。

```text
domain
  ↑
application（ユースケース / ポート）
  ↑
infrastructure（XDW / LZH / EMF / ローカルI/O）
  ↑
adapters（PDF / HTML / CLI）
```

ライブラリ利用者は主に `domain` と `application` を参照し、標準構成の具体実装は `infrastructure::local_service()` から取得します。各層の責務は [`docs/architecture.md`](docs/architecture.md) にまとめています。

## インストール

ソースから CLI をインストールします。

```sh
cargo install --path .
```

ビルドだけ行う場合は `cargo build --release` を使用してください。

## CLI

```text
xdw-salvage info     <FILE>...              構造とページ台帳を表示
xdw-salvage extract  <FILE>... -o <DIR>     JPEG ページと同梱元ファイルを書き出す
xdw-salvage pdf      <FILE>... [-o <OUT>]   回収可能なページから PDF を作る
xdw-salvage html     <FILE>... [-o <OUT>]   自己完結 HTML を作る
xdw-salvage codec    <FILE>... -o <DIR>     未復号ストリームと CSV を書き出す
xdw-salvage triage   <PATH>... [-o <CSV>]   資産全体を分類して CSV に出す
xdw-salvage manifest <FILE>... [-o <CSV>]   正しい変換に必要なページ数を出す
xdw-salvage verify   <FILE.xdw> --pages <N> 変換後のページ数を照合する
```

`pdf` と `html` は、入力が 1 件なら `-o` に出力ファイルを指定できます。入力が複数件の場合、`-o` は出力ディレクトリになります。

### 主なオプション

| オプション | 内容 |
|---|---|
| `--skip-missing` | 回収できないページを出力しない。既定ではページ番号を保つため注記ページを出力する |
| `--no-decode` | ベンダー独自符号化の展開を無効にする |
| `--font <path.ttf>` | PDF に埋め込む TrueType フォントを指定する |
| `--paper <a4\|letter\|WxH>` | PDF の用紙を統一する。`WxH` は mm 単位 |
| `--previews` | 低解像度の縮小画像エントリも出力対象にする |
| `--lang <ja\|en>` | 回収できないページの注記言語を指定する |
| `--no-attach` | 同梱された元ファイルを PDF / HTML に含めない |
| `--carry-source` | 元の `.xdw` 全体を PDF / HTML に添付する |
| `--no-bookmarks` | PDF の未回収ページ用しおりを作らない |
| `--pages <N>` | `verify` に渡す変換後ファイルのページ数 |

## 出力の仕様

### PDF

- 回収した JPEG は `DCTDecode` としてそのまま埋め込むため、再エンコードによる画質劣化がありません。
- 回収できないページは、既定では用紙サイズを保った注記ページになります。`--skip-missing` で省略できます。
- 同梱された元ファイルは既定で PDF 添付になります。独自符号化のページが多い場合は `--carry-source` を併用すると原本も一緒に保持できます。
- EMF から復元した文字は検索・コピーできます。配布先のフォント環境に依存しない PDF が必要な場合は `--font` を指定してください。
- ページサイズは入力の宣言値を使います。`--paper a4` や `--paper letter` を指定すると全ページを統一できます。

### HTML

外部ファイルを参照しない 1 ファイルの HTML を生成します。画像は data URL、同梱元ファイルはダウンロードリンクとして埋め込まれます。ページの回収方針は PDF と同じです。

## 移行監査

変換後の PDF を本クレート自身に再解析させるのではなく、コンテナから得られる期待値と外部ツールの結果を比較します。

```sh
xdw-salvage manifest 稟議書.xdw
converted_pages="$(pdfinfo 稟議書.pdf | awk '/^Pages/{print $2}')"
xdw-salvage verify 稟議書.xdw --pages "$converted_pages"
```

ページ数が一致しない場合、`verify` は終了コード 1 を返します。大量の入力を調べる場合は `triage`、個別の未復号ストリームを保存する場合は `codec` を使用してください。

## ライブラリとして使う

```rust
use std::path::Path;

use xdw_salvage::{adapters::pdf, infrastructure};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = infrastructure::local_service();
    let asset = service.open(Path::new("scan.xdw"))?;
    let analysis = service.analyze(&asset);

    println!(
        "{} ページ / ページ上の画像 {} 点（うち {} 点を回収）",
        analysis.coverage.sheets,
        analysis.coverage.pictures,
        analysis.coverage.pictures_recovered,
    );

    for attachment in &analysis.attachments {
        let name = format!("original.{}", attachment.kind.extension());
        std::fs::write(name, attachment.bytes(&asset.data))?;
    }

    let (pdf_bytes, report) = pdf::build(
        &asset.data,
        &asset.document,
        pdf::Options::default(),
    );
    std::fs::write("scan.pdf", pdf_bytes)?;
    println!("{} ページを PDF に埋め込み", report.embedded);
    Ok(())
}
```

別の入力元やデコーダを使う場合は、`application::ports` のポートを実装し、`SalvageService::new` と `adapters::pdf::build_with` / `adapters::html::build_with` に渡せます。

## 回収範囲と制限

- 写真・図版として格納された JPEG は、元のバイト列を保ったまま回収します。連続する画像帯は、ページ上で扱いやすい単位に連結します。
- プリンタドライバ由来の版面層は、LHA `-lh5-` を展開して EMF または WMF として解釈します。文字、JPEG の配置、埋め込み DIB（1/4/8/24 bpp、RLE4/RLE8）、`PATCOPY` の塗り、private のパス（クリップ・塗り・縁取り・ベジェ）を再現します。手元の 7 文書 17 ページは、ベンダー製ソフトの PDF 出力と見た目で一致します。
- 画像は再エンコードしません。埋め込み DIB は依存なしの zlib 実装で `FlateDecode`（PDF）/ PNG（HTML）に格納します。
- 対応していないもの：破線などのペンスタイル、`DWc` の部分矩形（行方向以外）、WMF で送り幅を持たない文字列の正確な字送り。
- 回収できないページは推測で埋めず、`info` や出力レポートで明示します。
- パスワード保護・電子署名付きの文書は対象外です。アクセス制御を迂回する機能は実装していません。
- 壊れた入力や未知の形式は、成功またはエラーとして扱い、パニック・無限ループ・過大なメモリ確保を避けます。

コンテナ形式、LHA / EMF の解析、ページ表の判定根拠は [`docs/coding.md`](docs/coding.md) に、レイヤーの責務は [`docs/architecture.md`](docs/architecture.md) に記録しています。

## 開発

```sh
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

## 由来とライセンス

実ファイルの観察と、公開されているインターフェース仕様の意味論をもとに独自実装しています。ベンダーのコード、ヘッダ、バイナリ、文書は含めず、アクセス制御の迂回も行いません。テスト用のサンプル文書も同梱していません。

Unlicense。詳細は [`LICENSE`](LICENSE) を参照してください。

## English summary

`xdw-salvage` reads DocuWorks `.xdw` / `.xbd` containers and salvages data without the vendor's software: JPEG pages byte for byte, embedded source files, and printer-driver pages redrawn from the EMF or WMF inside their vendor coding (text, pictures, embedded bitmaps, fills, clip paths and outlines), with page rotation and annotations taken from the document properties. It can write PDF or self-contained HTML and provides commands for inventory and migration checks. Protected and signed documents are refused; no access-control bypass is implemented.
