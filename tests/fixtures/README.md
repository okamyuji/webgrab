# テスト fixture

- `big_gzip.html.gz`: 展開後約2MiBになる本文（`x`の繰り返し + 番兵`SENTINEL_GZIP_9f3c`）を`gzip -9`で圧縮した約2.1KiBのバイナリ。E13とE14はこれを`Content-Encoding: gzip`として配信し、`--max-bytes`が展開後バイト数を基準に判定することを確認する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>gz</title></head><body><article><p>'+'x'*2097152+' SENTINEL_GZIP_9f3c</p></article></body></html>')" | gzip -9 > tests/fixtures/big_gzip.html.gz
```
- `article.html.br`、`article.html.zst`: 番兵`SENTINEL-BR-4d1e`と`SENTINEL-ZSTD-7b2a`を含む短い記事を`brotli -9`と`zstd -19`で圧縮したバイナリ。静的取得が`Content-Encoding: br`の応答を展開して出力し、展開しない`zstd`の応答を終了コード4で拒否することを確認する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>Brotli article</title></head><body><article><h1>Brotli article</h1><p>This paragraph is long enough for the extraction algorithm to treat it as the main content of the page, so the decoded text reaches the output. It continues with a few more sentences to add weight. SENTINEL-BR-4d1e</p></article></body></html>')" | brotli -9 -c > tests/fixtures/article.html.br
python3 -c "import sys;sys.stdout.write('<html><head><title>Zstd article</title></head><body><article><h1>Zstd article</h1><p>This paragraph is long enough for the extraction algorithm to treat it as the main content of the page, so the decoded text reaches the output. It continues with a few more sentences to add weight. SENTINEL-ZSTD-7b2a</p></article></body></html>')" | zstd -19 -c > tests/fixtures/article.html.zst
```
- `big_br.html.br`: 展開後約2MiBになる本文を`brotli -9`で圧縮した200バイト未満のバイナリ。`--max-bytes`が展開後バイト数を基準に判定することを確認する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>br</title></head><body><article><p>'+'x'*2097152+' SENTINEL_BR_BIG_5a1c</p></article></body></html>')" | brotli -9 -c > tests/fixtures/big_br.html.br
```
- `robots_disallow.txt.br`、`robots_over_cap.txt.br`: `Disallow: /`だけの短い robots.txt と、同じ内容に約600KB（512KiB超）の注釈行を足した robots.txt を`brotli -9`で圧縮したバイナリ。robots.txt が展開後に解釈され、512KiBの上限も展開後の大きさに掛かることを確認する。再生成コマンドは次のとおり。

```sh
printf 'User-agent: *\nDisallow: /\n' | brotli -9 -c > tests/fixtures/robots_disallow.txt.br
python3 -c "import sys;sys.stdout.write('User-agent: *\nDisallow: /\n#'+'x'*600000+'\n')" | brotli -9 -c > tests/fixtures/robots_over_cap.txt.br
```
