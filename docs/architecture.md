# アーキテクチャ

このクレートは、XDW の読み取り処理と出力形式を直接結び付けないように、次の依存方向で構成しています。

```text
domain
  ↑
application (ports / recovery use cases / SalvageService)
  ↑
infrastructure (XDW parser / LZH+EMF decoder / attachment scanner / local I/O)
  ↑
adapters (PDF / HTML / CLI)
```

## domain

`domain::document` と `domain::page` は、文書・ページの値モデルと構造上の集計を持ちます。`Page` は文書プロパティ由来の回転角と、ページに重なる図形・注釈（`Overlay`）も持ちます。`domain::rendering` は、復号済みページを表すテキスト・画像配置・内包ラスタ・塗りつぶし・パス（クリップ、塗り、縁取り）のモデルで、座標はすべてデバイス画素に揃えてあります。いずれも PDF や HTML、XDWのTLV読取処理には依存しません。`domain::policy` は、添付ファイルの有無と復元結果から移行判定を計算します。

利用者は `xdw_salvage::domain` のモデルと `xdw_salvage::application` のユースケースを参照します。ドメインモデルにはファイルI/Oや解析処理を持たせていません。

## application

`application::ports` に、文書入力、コンテナ解析、添付検出、コード化ページ復号（ページ本体と、ページに重なる図形の両方）のインターフェースを置いています。`SalvageService` はこれらを組み合わせて、読み込みと分析を実行します。したがって、メモリ入力や別のコーデックを使うテスト・アプリケーションがローカルファイルシステムに依存する必要はありません。

`application::recovery` は、PDF と HTML が共有する「空のメタファイルは復元失敗とみなす」「復元済みページ数を一度の規則で数える」というユースケースを持ちます。

`application::verification` は、コンテナから得た期待ページ数・用紙サイズと、外部のPDFツールが報告した結果を比較します。PDFの読み取り自体は担当せず、外部ツールへの依存をアプリケーション層へ持ち込みません。

## infrastructure

具体的な XDWコンテナ／ページ解析、LZH実装、埋め込みファイル検出、ローカルファイル読み込みは `infrastructure` に集約しています。標準構成は `infrastructure::local_service()` で生成できます。`xdw_document` と `xdw_page` は解析結果をドメインモデルへ変換する境界で、`xdw_properties` は文書プロパティブロックから回転・表示用紙・注釈を取り出します。

メタファイルは `emf`（32 ビット形式）と `wmf`（16 ビット形式）が読み、共通の GDI 状態（座標変換、オブジェクト、クリップ、パス、メーカー private コメント）は `gdi::Canvas` にまとめています。`dib` は内包ビットマップを展開し、`deflate` と `png` は出力側が画像を格納するための依存なしの圧縮器です。

## adapters

PDF / HTML の実装本体は `adapters::pdf` / `adapters::html` に置いています。依存を差し替える場合は `build_with` に `PageDecoder` と `AttachmentScanner` を渡します。CLI は `SalvageService` と `build_with` を利用し、形式解析・復号・出力の責務を分離しています。
