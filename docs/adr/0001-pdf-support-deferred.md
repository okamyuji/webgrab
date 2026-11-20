# ADR-0001 PDF対応の見送り

- 状態: 採択
- 日付: 2026-11-21
- 関連: [04-design.md](../04-design.md) §10、[10-fetch-fidelity-design.md](../10-fetch-fidelity-design.md) §2

## 状況

webgrabが受け付けるContent-Typeは`text/html`、`application/xhtml+xml`、`text/plain`の3種類です。Content-Typeが`application/pdf`の応答は終了コード4で拒否します。Content-Typeが欠落した応答はHTML経路で扱うため、この拒否の対象になりません。論文や仕様書はPDFで配布されることが多いため、PDFのテキストをMarkdownで返す機能を検討しました。

webgrabはMITライセンスで配布します。このため依存クレートには静的リンクしてもMITで配布できるライセンスが必要です。crates.ioから取得するクレートだけでビルドが完結することも条件にしました。cargoだけでビルドと配布を済ませるためです。

## 調査結果

調査日は2026-11-21です。各候補のライセンスはcrates.ioのメタデータとリポジトリのライセンスファイルで確認しました。pdf-extractのリポジトリにはライセンスファイルが無く、crates.ioの宣言だけが根拠です。推移的依存のライセンスは空のクレートに各候補を既定のfeatureで追加し、`cargo tree -e normal --format '{p} {l}'`で確かめました。表に挙げたクレートでは推移的依存にGPL、LGPL、AGPL、MPLは含まれていませんでした。

表の版は調査日の時点で公開から3日以上経っていた最新版です。pdf-inspectorには1.27.0も公開されていましたが、公開から1日未満でした。

| クレート | 版 | ライセンス | Markdown出力 | CJK | 悪意あるPDFへの備え | 外部ライブラリ |
|---|---|---|---|---|---|---|
| [pdf-inspector](https://github.com/firecrawl/pdf-inspector) | 1.26.1 | MIT | あり | ToUnicode CMapに対応 | メモリ枯渇の問題が未修正 | 不要 |
| [oxidize-pdf](https://github.com/bzsanti/oxidizePdf) | 5.6.1 | MIT | あり | READMEに対応を明記 | READMEに展開爆弾への保護を明記。実効性は未検証 | 不要 |
| [pdf-extract](https://github.com/jrmuizel/pdf-extract) | 0.12.1 | MIT | なし（プレーンテキスト） | 未確認 | panicとstack overflowの未解決issueが多数 | 不要 |
| [lopdf](https://github.com/J-F-Liu/lopdf) | 0.47.0 | MIT | なし（低レベルのパーサ） | 未確認 | 未確認 | 不要 |
| [pdfium-render](https://github.com/ajrcarey/pdfium-render) | 0.9.5 | MIT OR Apache-2.0 | なし | 未確認 | 未確認 | Pdfiumのライブラリが必要 |

mupdfのRustバインディングはAGPLであり、MITで配布するwebgrabからは静的リンクできません。このため候補から外しました。

pdf-inspectorはMarkdownを直接出力するため、webgrabの出力形式に最も近い候補でした。ただし次の2件が未修正のまま残っています。

- [firecrawl/pdf-inspector#641](https://github.com/firecrawl/pdf-inspector/issues/641) OCRが必要なページかを判定する処理が、content streamを上限なしで展開します。報告では1.25.2で4.2MBのPDFが4GiBに展開され、メモリ確保に失敗してプロセスがabortしました。展開量を制限する修正が[firecrawl/pdf-inspector#654](https://github.com/firecrawl/pdf-inspector/pull/654)として提案されていますが、調査日の時点では未マージです。
- [firecrawl/pdf-inspector#676](https://github.com/firecrawl/pdf-inspector/issues/676) ページの`/Resources`を共有するForm XObjectが数千個あると、メモリ使用量が際限なく増えます。報告では1.26.1でOSによる強制終了に至り、0.7.2ではsegfaultが起きました。

## 決定

04-design.md §10のとおり、PDF対応は実装しません。PDFを非HTML形式の対象外とする扱いを維持します。

## 理由

webgrabは信頼できないWebページを取得します。このため、悪意あるPDFを受け取ることを前提に設計する必要があります。#641ではメモリ確保の失敗によってプロセスがabortし、`catch_unwind`ではこのabortを捕捉できません。#676でもメモリの増加はOSが強制終了するまで止まりません。

既存の`--max-bytes`はHTTP応答の本体をContent-Encodingの展開後に数えた量を制限します。PDFの内部にある圧縮ストリームを展開した後のメモリは、この制限の対象外です。#641の4.2MBのPDFは既定の上限20MiBに収まるため、`--max-bytes`ではこの問題を防げません。

この問題を防ぐには、PDFの解析を子プロセスに分け、メモリ上限と時間上限を掛ける必要があります。この対策を入れると、取得経路に子プロセスの起動と結果の受け渡しが加わります。さらに、子プロセスの異常終了を既存の終了コード表へ対応づける必要もあります。webgrabは1回の実行で1ページを返す単純なCLIです。この規模に対して、加わる分岐と試験のコストは大きいと判断しました。

## 採らなかった案

- pdf-inspectorを子プロセスなしで組み込む案 悪意あるPDFを1つ受け取るだけで、メモリ枯渇とabortが起こりえます。信頼できない入力を扱うツールとしては受け入れられないため、採りませんでした。
- pdf-inspectorを子プロセスに分けて組み込む案 前節で述べたとおり、加わる分岐と試験のコストが機能の規模に見合わないため、採りませんでした。
- oxidize-pdfを使う案 READMEは展開爆弾への保護を主張しています。しかし、#641や#676と同じ形のPDFでその保護が働くかは検証していません。抽出品質も実測していないため、採用の根拠が足りないと判断しました。
- lopdfを使う案 低レベルのパーサであり、レイアウトを保ったテキスト抽出とMarkdown変換を自前で実装する必要があります。実装量が大きいため、採りませんでした。
- pdf-extractを使う案 panicとstack overflowの未解決issueが多く、信頼できない入力を扱うには不向きなため、採りませんでした。
- pdfium-renderを使う案 静的リンクは可能ですが、Pdfiumの静的ライブラリを別途入手するかビルドする必要があります。crates.ioだけでビルドが完結しないため、採りませんでした。

## 再検討の条件

次のどちらかが成り立った時点で、この決定を見直します。

- pdf-inspectorで#641と#676が修正されたとき。修正版に#641と#676の再現用PDFを渡し、プロセスが終了せず、メモリ使用量が`--max-bytes`の既定値の数倍以内に収まることを実行して確認します。
- oxidize-pdfに同じ再現用PDFを渡し、同じ基準を満たすことを実行して確認できたとき。

どちらの場合も日本語を含む実際のPDFで抽出結果を比べてから設計に入ります。

## 影響

Content-Typeが`application/pdf`の応答は、従来どおり終了コード4で拒否されます。PDFの内容が必要な利用者は別のツールで取り出す必要があります。
