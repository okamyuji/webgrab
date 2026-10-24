# テスト fixture

- `big_gzip.html.gz`: 展開後約2MiBになる本文（`x`の繰り返し + 番兵`SENTINEL_GZIP_9f3c`）を`gzip -9`で圧縮した約2.1KiBのバイナリ。E13とE14はこれを`Content-Encoding: gzip`として配信し、`--max-bytes`が展開後バイト数を基準に判定することを確認する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>gz</title></head><body><article><p>'+'x'*2097152+' SENTINEL_GZIP_9f3c</p></article></body></html>')" | gzip -9 > tests/fixtures/big_gzip.html.gz
```
- `article.html.br`: 番兵`SENTINEL_BR_4d1e`を含む短い記事を`brotli -9`で圧縮したバイナリ。静的取得が`Content-Encoding: br`の応答を展開せずに拒否することを確認する。再生成コマンドは次のとおり。

```sh
python3 -c "import sys;sys.stdout.write('<html><head><title>br</title></head><body><article><p>'+'Brotli body. '*40+'SENTINEL_BR_4d1e</p></article></body></html>')" | brotli -9 -c > tests/fixtures/article.html.br
```
